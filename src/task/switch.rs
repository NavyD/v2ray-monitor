use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashSet},
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use crate::{
    tcp_ping::TcpPingStatistic,
    v2ray::{node::Node, V2rayService},
};
use anyhow::{anyhow, Result};

use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use reqwest::{Client, Proxy};
use tokio::sync::mpsc::Receiver;

use super::{
    filter::{Filter, *},
    v2ray_task_config::SwitchTaskProperty,
    RetryService,
};

use pnet::datalink::Channel::Ethernet;
use pnet::packet::ip::IpNextHeaderProtocols;
use pnet::packet::ipv4::Ipv4Packet;
use pnet::packet::tcp::TcpPacket;
use pnet::packet::Packet;

use pnet::{
    datalink::{self, DataLinkReceiver},
    packet::ethernet::{EtherTypes, EthernetPacket},
};

#[derive(Clone, Debug)]
pub struct SwitchNodeStat {
    pub node: Node,
    pub tcp_stat: TcpPingStatistic,
    pub serv_duras: Vec<Option<Duration>>,
}

impl SwitchNodeStat {
    pub fn new(node: Node, ps: TcpPingStatistic) -> Self {
        Self {
            node,
            tcp_stat: ps,
            serv_duras: vec![],
        }
    }

    pub fn push_serv_duration(&mut self, d: Option<Duration>) {
        self.serv_duras.push(d);
    }

    pub fn weight(&self) -> usize {
        if let Some(avg) = self.tcp_stat.rtt_avg {
            let total = self.serv_duras.len();
            let avg = avg.as_millis() as usize;
            if total == 0 {
                return avg;
            }
            avg * (total + 1)
        } else {
            std::usize::MAX
        }
    }
}

impl Eq for SwitchNodeStat {}

impl PartialEq for SwitchNodeStat {
    fn eq(&self, other: &Self) -> bool {
        self.weight() == other.weight() && self.node == other.node
    }
}

impl std::cmp::Ord for SwitchNodeStat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        fn cmptest(this: &SwitchNodeStat, other: &SwitchNodeStat) -> std::cmp::Ordering {
            let r = this.weight().cmp(&other.weight());
            if r != Ordering::Equal {
                return r;
            }
            if this.node == other.node {
                return Ordering::Equal;
            }

            let compare = |a: Option<Duration>, b: Option<Duration>| match (a, b) {
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (a, b) => a.cmp(&b),
            };
            let v = compare(this.tcp_stat.rtt_avg, other.tcp_stat.rtt_avg);
            if v != Ordering::Equal {
                return v;
            }
            let v = compare(this.tcp_stat.rtt_min, other.tcp_stat.rtt_min);
            if v != Ordering::Equal {
                return v;
            }
            compare(this.tcp_stat.rtt_max, other.tcp_stat.rtt_max)
        }
        cmptest(self, other).reverse()
    }
}

impl std::cmp::PartialOrd for SwitchNodeStat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

pub type SwitchData = Arc<Mutex<BinaryHeap<SwitchNodeStat>>>;

pub struct SwitchTask<V: V2rayService> {
    stats: SwitchData,
    v2: V,
    pre_filter: Option<Arc<NameRegexFilter>>,
    select_filter: Arc<SwitchSelectFilter>,
    prop: SwitchTaskProperty,
    check_client: OnceCell<Client>,
    check_ips: Arc<Mutex<HashSet<IpAddr>>>,
    check_retry_srv: RetryService,
}

impl<V: V2rayService> SwitchTask<V> {
    pub fn new(prop: SwitchTaskProperty, v2: V) -> Self {
        Self {
            stats: Arc::new(Mutex::new(BinaryHeap::new())),
            v2,
            pre_filter: prop
                .filter
                .name_regex
                .as_ref()
                .map(|s| Arc::new(NameRegexFilter::new(s))),
            select_filter: Arc::new(SwitchSelectFilter::new(prop.filter.lb_nodes_size.into())),
            check_client: OnceCell::new(),
            check_ips: Arc::new(Mutex::new(HashSet::new())),
            check_retry_srv: RetryService::new(prop.check_retry.clone()),
            prop,
        }
    }

    /// 等待首次接收node stats数据并无限循环切换节点
    pub async fn run(
        &self,
        node_stats_rx: Receiver<Vec<(Node, TcpPingStatistic)>>,
        ips_rx: Receiver<Vec<IpAddr>>,
    ) -> Result<()> {
        tokio::try_join!(
            self.update_check_ips(ips_rx),
            self.update_node_stats(node_stats_rx)
        )?;

        let mut link_rev = self.find_link_receiver()?;
        self.v2.clean_env().await?;

        let monitor = &self.prop.monitor;
        // packet上次数据
        let (mut p_timeout_count, mut p_last_time) = (0, None::<SystemTime>);
        // switch上次数据
        let (mut last_nodes, mut last_time) = (None::<Vec<Node>>, None::<SystemTime>);

        // 统计数据
        let (mut switch_count, mut switch_failed_count, start) = (0, 0, SystemTime::now());

        loop {
            match link_rev.next() {
                Ok(packet) => {
                    if let Ok(is_forword) = get_packet_direction(packet, self.check_ips.clone()) {
                        // 收到回复
                        if !is_forword {
                            log::trace!("received reponse. reset timeout count: {} as 0, last elapsed: {:?} as None", 
                                p_timeout_count, 
                                p_last_time.as_ref().and_then(|t| t.elapsed().ok())
                            );
                            p_timeout_count = 0;
                            p_last_time.take();
                            continue;
                        }
                        // 发送 且 上次还未收到回复
                        if let Some(t) = p_last_time {
                            let elapsed = t.elapsed()?;
                            if elapsed < monitor.timeout {
                                continue;
                            }
                            p_timeout_count += 1;
                            if p_timeout_count < monitor.count {
                                log::trace!("No response found within duration: {:?}, timeout count: {}, limit count: {}", elapsed, p_timeout_count, monitor.count);
                                continue;
                            }
                            // switch limit for elapsed
                            if let Some(elapsed) = last_time.as_ref().and_then(|t| t.elapsed().ok()) {
                                let limit_interval = self.prop.limit_interval;
                                if elapsed < limit_interval {
                                    log::trace!(
                                        "Ignore frequent switching. elapsed: {:?}, switch limit: {:?}",
                                        elapsed,
                                        limit_interval
                                    );
                                    continue;
                                }
                            }
                            switch_count += 1;
                            // switch
                            if let Err(e) = self.switch(&mut last_nodes, &mut last_time).await {
                                switch_failed_count += 1;
                                log::error!("switch error: {}", e);
                            }
                            // 每10次报告一次
                            if switch_count % 10 == 0 {
                                log::info!("{} switchovers occurred within {:?} minutes, switch failed count: {}", switch_count, start.elapsed()?, switch_failed_count);
                            }
                        } else {
                            p_last_time.replace(SystemTime::now());
                        }
                    }
                }
                Err(e) => log::debug!("receive error {} for interface {}", e, monitor.ifname),
            }
        }
    }

    async fn switch(
        &self,
        last_nodes: &mut Option<Vec<Node>>,
        last_time: &mut Option<SystemTime>,
    ) -> Result<()> {
        log::debug!("Start switching nodes for last_nodes: {:?}, last_duration: {:?}", last_nodes, last_time.as_ref().and_then(|t| t.elapsed().ok()));
        self.repush_last(
            last_nodes.take(),
            last_time.take().and_then(|t| t.elapsed().ok()),
        )?;

        let nodes = self.select_filter.filter(self.stats.clone());
        if nodes.is_empty() {
            log::error!("Switch node error: no any nodes");
            return Err(anyhow!("no any nodes"));
        } else if log::log_enabled!(log::Level::Trace) {
            log::trace!(
                "switching nodes with {:?}",
                nodes
                    .iter()
                    .map(|node| node.remark.as_ref())
                    .collect::<Vec<_>>()
            );
        }

        // 切换节点
        self.v2
            .restart_in_background(
                &self
                    .v2
                    .gen_config(&nodes.iter().collect::<Vec<_>>())
                    .await?,
            )
            .await?;

        last_nodes.replace(nodes);
        last_time.replace(SystemTime::now());

        if let Err(e) = self
            .check_retry_srv
            .retry_on(|| self.check_network(), false)
            .await
        {
            log::warn!(
                "The network still fails: {}, after switching nodes: {:?}",
                e,
                last_nodes
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|n| n.remark.as_ref())
                    .collect::<Vec<_>>()
            );
            return Err(anyhow!("check network failed"));
        }

        if log::log_enabled!(log::Level::Info) {
            log::info!(
                "Node switch succeeded: {:?}",
                last_nodes
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|n| n.remark.as_ref())
                    .collect::<Vec<_>>()
            );
        }
        Ok(())
    }

    fn find_link_receiver(&self) -> Result<Box<dyn DataLinkReceiver>> {
        let ifname = &self.prop.monitor.ifname;
        log::trace!("Looking for available network cards by name: {}", ifname);
        let interface = datalink::interfaces()
            .into_iter()
            .find(|iface| iface.name == *ifname)
            .ok_or_else(|| anyhow!("not found interface with name: {}", ifname))?;
        // Create a channel to receive on
        match datalink::channel(&interface, Default::default()) {
            Ok(Ethernet(_, rx)) => Ok(rx),
            Ok(_) => Err(anyhow!("unhandled channel type for ifname: {}", ifname)),
            Err(e) => Err(e).map_err(Into::into),
        }
    }

    async fn update_check_ips(&self, mut ips_rx: Receiver<Vec<IpAddr>>) -> Result<()> {
        log::trace!("Waiting for the first update of ips");
        let ips = ips_rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("receive error"))?;

        let update = |ips: Vec<IpAddr>, check_ips: &Arc<Mutex<HashSet<IpAddr>>>| {
            log::debug!("Received ips: {:?}", ips);
            let mut cips = check_ips.lock();
            cips.clear();
            ips.into_iter().for_each(|ip| {
                cips.insert(ip);
            });
        };

        let check_ips = self.check_ips.clone();
        update(ips, &check_ips);
        tokio::spawn(async move {
            while let Some(ips) = ips_rx.recv().await {
                update(ips, &check_ips);
            }
        });
        Ok(())
    }

    /// 更新node stats为SwitchNodeStat。首次调用将会等待数据可用，之后会在后台更新。
    async fn update_node_stats(
        &self,
        mut rx: Receiver<Vec<(Node, TcpPingStatistic)>>,
    ) -> Result<()> {
        log::debug!("Waiting to receive nodes stats data");
        let node_stats = rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("receive nodes error"))?;
        log::debug!(
            "receive data from channel for the first time node stats: {}",
            node_stats.len()
        );

        // 更新stats
        let update = |stats: &SwitchData,
                      node_stats: Vec<(Node, TcpPingStatistic)>,
                      pre_filter: Option<&Arc<NameRegexFilter>>| {
            let node_stats = node_stats
                .into_iter()
                .map(|(n, ps)| SwitchNodeStat::new(n, ps))
                .collect::<BinaryHeap<_>>();

            *stats.lock() = if let Some(f) = pre_filter.as_ref() {
                f.filter(node_stats)
            } else {
                node_stats
            };
        };

        let stats = self.stats.clone();
        let pre_filter = self.pre_filter.clone();

        update(&stats, node_stats, pre_filter.as_ref());

        tokio::spawn(async move {
            log::info!("Waiting for nodes stats update in the background");
            while let Some(node_stats) = rx.recv().await {
                log::debug!("processing received node stats: {}", node_stats.len());
                update(&stats, node_stats, pre_filter.as_ref());
            }
        });
        Ok(())
    }

    async fn check_network(&self) -> Result<()> {
        let client = if let Some(v) = self.check_client.get() {
            v
        } else {
            let proxy_url = self.v2.get_proxy_url(self.v2.get_config().await?)?;
            self.check_client.get_or_try_init(|| {
                log::trace!(
                    "Initializing the check networking client with timeout: {:?}, proxy: {:?}",
                    self.prop.check_timeout,
                    proxy_url
                );
                proxy_url
                    .as_ref()
                    .map(|url| Proxy::all(url).map(|proxy| reqwest::Client::builder().proxy(proxy)))
                    .unwrap_or_else(|| Ok(reqwest::Client::builder()))?
                    .timeout(self.prop.check_timeout)
                    .build()
            })?
        };
        let url = &self.prop.check_url;
        let start = Instant::now();
        let status = client.get(url).send().await?.status();
        let elapsed = Instant::now() - start;
        if !status.is_success() {
            log::info!(
                "switch checking got exception status: {} for get request url: {}",
                status,
                url
            );
        }
        log::trace!(
            "Check the network {} successfully consumption: {:?}",
            url,
            elapsed
        );
        Ok(())
    }

    /// 将last_switched中的nodes结合last_duration重入stats中使节点重排序
    fn repush_last(
        &self,
        last_switched: Option<Vec<Node>>,
        last_duration: Option<Duration>,
    ) -> Result<()> {
        if let Some(mut last) = last_switched {
            log::trace!(
                "repush for last switched nodes size: {}, last duration: {:?}",
                last.len(),
                last_duration
            );
            let last_duration = last_duration.ok_or_else(|| {
                anyhow!(
                    "last duration: None is not consistent with last switched value: {:?}",
                    last
                )
            })?;
            let mut stats = self.stats.lock();
            let mut temp = vec![];
            while let Some(mut ns) = stats.pop() {
                if last.is_empty() {
                    stats.push(ns);
                    break;
                }
                if let Some(idx) =
                    last.iter()
                        .enumerate()
                        .find_map(|(i, n)| if n == &ns.node { Some(i) } else { None })
                {
                    ns.serv_duras.push(Some(last_duration));
                    temp.push(ns);

                    last.remove(idx);
                } else {
                    log::error!(
                        "not found switch node {:?} in last switched: {:?}",
                        ns.node.remark.as_ref(),
                        last.iter().map(|n| n.remark.as_ref()).collect::<Vec<_>>()
                    );
                    return Err(anyhow!("not found node in last switched"));
                }
            }

            temp.into_iter().for_each(|ns| stats.push(ns));
        }
        Ok(())
    }
}

fn get_packet_direction(packet: &[u8], ips: Arc<Mutex<HashSet<IpAddr>>>) -> Result<bool, ()> {
    let ethernet = EthernetPacket::new(packet).ok_or(())?;
    match ethernet.get_ethertype() {
        EtherTypes::Ipv4 => {
            let header = Ipv4Packet::new(ethernet.payload()).ok_or(())?;
            let source = IpAddr::V4(header.get_source());
            let destination = IpAddr::V4(header.get_destination());
            let protocol = header.get_next_level_protocol();
            match protocol {
                IpNextHeaderProtocols::Tcp => {
                    let ips = ips.lock();
                    if ips.contains(&source) {
                        let tcp = TcpPacket::new(header.payload()).ok_or(())?;
                        // 在dns查询后tcp rst连接重置 相当无法连接
                        if tcp.get_flags() & 4 != 0 {
                            log::trace!("{} RST: {} <- {}", protocol, destination, source);
                            Err(())
                        } else {
                            log::trace!("{}: {} <- {}", protocol, destination, source);
                            Ok(false)
                        }
                    } else if ips.contains(&destination) {
                        log::trace!("{}: {} -> {}", protocol, source, destination);
                        Ok(true)
                    } else {
                        Err(())
                    }
                }
                _ => Err(()),
            }
        }
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {

    use once_cell::sync::{Lazy, OnceCell};
    use tokio::{
        sync::mpsc::channel,
        time::{sleep, timeout},
    };

    use crate::{
        task::v2ray_task_config::{PingProperty, V2rayProperty},
        tcp_ping,
        v2ray::{node, LocalV2ray},
    };

    static V2: Lazy<LocalV2ray> = Lazy::new(|| LocalV2ray::new(get_v2ray_prop().unwrap().local));

    use super::*;
    use crate::v2ray::ConfigurableV2ray;

    #[tokio::test]
    async fn check_network_normal_and_timeout_after_v2_startup() -> Result<()> {
        let task = get_updated_task().await?;
        let mut node = task.stats.lock().peek().map(|ns| ns.node.clone()).unwrap();

        {
            // normal
            let config = task.v2.gen_config(&[&node]).await?;
            let _c = task.v2.start(&config).await?;
            task.check_network().await?;
        }

        node.add.replace("test.add".to_string());

        let config = task.v2.gen_config(&[&node]).await?;
        let _c = task.v2.start(&config).await?;

        // client设置的超时正常超时退出
        let timeout_du = task.prop.check_timeout + Duration::from_millis(50);
        let res = timeout(timeout_du, task.check_network()).await?;
        assert!(res.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn orderly_update_nodes() -> Result<()> {
        let task = get_updated_task().await?;
        // 测试有序
        let first = task.stats.lock().pop().unwrap();
        let second = task.stats.lock().pop().unwrap();
        assert!(first.tcp_stat < second.tcp_stat);
        Ok(())
    }

    // #[tokio::test]
    // // #[ignore]
    // async fn switch_normal_nodes() -> Result<()> {
    //     let task = get_updated_task().await?;
    //     assert!(task.check_network().await.is_err());
    //     let nodes = task.switch_nodes().await?;
    //     assert!(!nodes.is_empty(), "switched nodes has empty");
    //     task.check_network().await?;
    //     task.v2.stop_all().await?;
    //     Ok(())
    // }

    #[tokio::test]
    // #[ignore]
    async fn update_weight_after_repush() -> Result<()> {
        let task = get_updated_task().await?;
        let old_len = task.stats.lock().len();
        assert!(old_len >= 2);
        let first_node = task.stats.lock().peek().unwrap().clone();
        let last_switched = Some(vec![first_node.node.clone()]);
        let last_duration = Some(Duration::from_millis(100));
        task.repush_last(last_switched, last_duration)?;
        // 不修改数量
        assert_eq!(task.stats.lock().len(), old_len);
        // 修改weight
        let updated_first = task
            .stats
            .lock()
            .iter()
            .find(|ns| ns.node == first_node.node)
            .cloned()
            .unwrap();
        assert_ne!(updated_first.weight(), first_node.weight());
        Ok(())
    }

    #[tokio::test]
    #[ignore]
    async fn run_test() -> Result<()> {
        let (stats_tx, _stats_rx) = channel(1);
        let switch = get_switch_prop()?;
        let nodes = get_node_stats().await?;

        let task = SwitchTask::new(switch, V2.clone());
        assert!(task.stats.lock().is_empty());

        stats_tx.send(nodes.to_vec()).await?;
        // task.run(stats_rx).await?;
        sleep(Duration::from_secs(4)).await;

        task.check_network().await?;
        task.v2.stop_all().await?;
        Ok(())
    }

    async fn get_updated_task() -> Result<SwitchTask<LocalV2ray>> {
        let (stats_tx, stats_rx) = channel(1);
        let switch = get_switch_prop()?;
        let nodes = get_node_stats().await?;

        let task = SwitchTask::new(switch, V2.clone());
        assert!(task.stats.lock().is_empty());

        stats_tx.send(nodes.to_vec()).await?;
        task.update_node_stats(stats_rx).await?;
        assert_eq!(task.stats.lock().len(), nodes.len());
        Ok(task)
    }

    async fn get_node_stats() -> Result<Vec<(Node, TcpPingStatistic)>> {
        static NODE_STATS: OnceCell<Mutex<Vec<(Node, TcpPingStatistic)>>> = OnceCell::new();
        let mut ns = NODE_STATS.get_or_init(|| Mutex::new(vec![])).lock();
        if !ns.is_empty() {
            return Ok(ns.to_vec());
        }
        let nodes = get_nodes().await?;
        let v2 = V2.clone();
        let (nodes, _) = tcp_ping::ping_batch(v2, nodes.clone(), &get_tcp_ping_prop()?).await?;
        assert!(nodes.len() > 1, "accessible node len: {}", nodes.len());
        *ns = nodes;
        Ok(ns.to_vec())
    }

    async fn get_nodes() -> Result<Vec<Node>> {
        let swith = get_switch_prop()?;
        let filter = swith.filter.name_regex.as_deref().map(NameRegexFilter::new);
        node::load_subscription_nodes_from_file("tests/data/v2ray-subscription.txt")
            .await
            .map(|nodes| {
                if let Some(f) = filter {
                    f.filter(nodes)
                } else {
                    nodes
                }
            })
    }

    fn get_tcp_ping_prop() -> Result<PingProperty> {
        let content = r#"
count: 3
ping_url: https://www.google.com/gen_204
timeout: 1s 500ms
concurr_num: 10"#;
        serde_yaml::from_str::<PingProperty>(content).map_err(Into::into)
    }

    // #[tokio::test]
    // async fn basic() -> Result<()> {
    //     let prop = get_switch_prop()?;
    //     let v2 = LocalV2ray::new(get_v2ray_prop()?.local);
    //     // let _task = SwitchTask::new(prop, v2);

    //     // task.run().await?;
    //     Ok(())
    // }

    fn get_v2ray_prop() -> Result<V2rayProperty> {
        let content = r#"{}"#;
        serde_yaml::from_str::<V2rayProperty>(content).map_err(Into::into)
    }

    fn get_switch_prop() -> Result<SwitchTaskProperty> {
        let content = r#"
check_url: https://www.google.com/gen_204
check_timeout: 2s
filter:
    lb_nodes_size: 3
    name_regex: "专线"
        "#;
        serde_yaml::from_str::<SwitchTaskProperty>(content).map_err(Into::into)
    }
}

use std::{
    borrow::Borrow,
    cmp::Ordering,
    collections::BinaryHeap,
    ops::Deref,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    tcp_ping::TcpPingStatistic,
    v2ray::{node::Node, V2rayService},
};
use anyhow::{anyhow, Result};

use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;
use reqwest::{Client, Proxy};
use tokio::{sync::mpsc::Receiver, task::JoinHandle};

use super::{
    filter::{Filter, *},
    v2ray_task_config::SwitchTaskProperty,
    RetryService,
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
            avg * total
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
type SelectFilter = Arc<Box<dyn Filter<SwitchData, Vec<SwitchNodeStat>>>>;
pub struct SwitchTask<V: V2rayService> {
    stats: SwitchData,
    v2: V,
    pre_filter: Option<Arc<NameRegexFilter>>,
    select_filter: SelectFilter,
    prop: SwitchTaskProperty,
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
            select_filter: Arc::new(Box::new(SwitchSelectFilter::new(
                prop.filter.lb_nodes_size.into(),
            ))),
            prop,
        }
    }

    pub async fn run(&self, rx: Receiver<Vec<(Node, TcpPingStatistic)>>) -> Result<()> {
        self.update_node_stats(rx).await?;

        let stats = self.stats.clone();
        let select_filter = self.select_filter.clone();
        let v2 = self.v2.clone();
        let retry_srv = RetryService::new(self.prop.retry.clone());
        let switch = self.prop.clone();

        let task = {
            let proxy_url = self.v2.get_proxy_url(self.v2.get_config().await?)?;
            let check_url = switch.check_url.clone();
            let timeout = switch.check_timeout;
            Arc::new(move || check_networking(check_url.clone(), timeout, proxy_url.clone()))
        };

        v2.clean_env().await?;
        tokio::spawn(async move {
            let (mut last_switched, mut last_duration, mut last_checked) =
                (None::<Vec<SwitchNodeStat>>, None::<Duration>, false);

            loop {
                log::debug!("retrying check networking");
                match retry_srv
                    .retry_on(task.clone().as_ref(), last_checked)
                    .await
                {
                    Ok((retries, duration)) => {
                        // 这次做成功时重试 后失败退出
                        if last_checked {
                            log::debug!(
                            "Found network problems. Reconfirming the problem after checking {} times of {:?}",
                            retries,
                            duration
                        );
                            last_duration = Some(duration);
                        }
                        // 上次是失败时 这次是做失败时重试 后成功退出
                        else {
                            log::debug!("After {} retries and {:?}, it is checked that the network has recovered", retries, duration);
                        }
                    }
                    // 重试次数达到max_retries
                    Err(e) => {
                        if !last_checked {
                            log::info!("trying to switch tangents, Confirm that there is a problem: {}, last_checked: {}, last_switched: {:?}, last_duration: {:?}", e, last_checked, last_switched, last_duration);
                            
                            if let Err(e) = switch_node(
                                &stats.clone(),
                                select_filter.clone(),
                                &v2,
                                last_switched.take(),
                                last_duration.take(),
                            )
                            .await
                            {
                                log::debug!("Node switch succeeded: {:?}", selected);
                            } else {
                                continue;
                            }
                        } else {
                            log::debug!(
                                "Check that a failure has occurred: {}, try again if it fails",
                                e
                            );
                        }
                    }
                }
                last_checked = !last_checked;
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
}

static CHECK_CLIENT: OnceCell<Client> = OnceCell::new();

async fn check_networking(url: String, timeout: Duration, proxy_url: Option<String>) -> Result<()> {
    let client = CHECK_CLIENT.get_or_try_init(|| {
        log::trace!(
            "Initializing the check networking client with timeout: {:?}, proxy: {:?}",
            timeout,
            proxy_url
        );
        proxy_url
            .as_ref()
            .map(|url| Proxy::all(url).map(|proxy| reqwest::Client::builder().proxy(proxy)))
            .unwrap_or_else(|| Ok(reqwest::Client::builder()))?
            .timeout(timeout)
            .build()
    })?;
    log::trace!(
        "Checking network url: {}, timeout: {:?}, proxy_url: {:?}",
        url,
        timeout,
        proxy_url
    );
    let start = Instant::now();
    let status = client.get(&url).send().await?.status();
    let elapsed = Instant::now() - start;
    if !status.is_success() {
        log::info!("switch checking got exception status: {}", status);
    }
    log::trace!(
        "Check the network {} successfully consumption: {:?}",
        url,
        elapsed
    );
    Ok(())
}

/// 
async fn switch_node<'a>(
    stats: &'a SwitchData,
    select_filter: SelectFilter,
    v2: &'a impl V2rayService,
    last_switched: &mut Option<Vec<SwitchNodeStat>>,
    last_dura: &mut Option<Duration>,
) -> Result<()> {
    // 将上次切换的节点重入 stats 权重排序
    if let Some(last) = last_switched {
        log::debug!("repush for last switched nodes size: {}", last.len());
        let mut stats = stats.lock();
        for mut stat in last {
            log::trace!(
                "repush node {:?}, old service duration: {:?}, cur switch duration: {:?}",
                stat.node.remark,
                stat.serv_duras,
                last_dura
            );
            stat.push_serv_duration(last_dura);
            stats.push(stat);
        }
    }

    // 从stats中取出节点
    let selected = select_filter.filter(stats.clone());
    if selected.is_empty() {
        log::error!("Switch node error: no any nodes");
        return Err(anyhow!("no any nodes"));
    }

    // 切换节点
    let config = v2
        .gen_config(
            &selected
                .iter()
                .map(|s| s.node.clone())
                .collect::<Vec<_>>()
                .iter()
                .collect::<Vec<_>>(),
        )
        .await?;

    v2.restart_in_background(&config).await?;

    if log::log_enabled!(log::Level::Info) {
        let nodes_msg = selected
            .iter()
            .map(|v| (v.node.remark.as_ref(), v.tcp_stat.rtt_avg.as_ref()))
            .collect::<Vec<_>>();
        log::info!("selected nodes: {:?}", nodes_msg);
    }

    last_switched.replace(selected);
    
    let mut stats = stats.lock();
    selected.into_iter().for_each(|n| stats.push(n));
    Ok(())
}

#[cfg(test)]
mod tests {

    use std::{fs::read_to_string, future::Future, task::Context, thread};

    use futures::future;
    use tokio::{runtime::Handle, sync::mpsc::channel, time::timeout};

    use crate::{
        task::{
            find_v2ray_bin_path,
            v2ray_task_config::{LocalV2rayProperty, PingProperty, V2rayProperty},
        },
        tcp_ping,
        v2ray::{node, LocalV2ray},
    };

    static V2: Lazy<LocalV2ray> = Lazy::new(|| LocalV2ray::new(get_v2ray_prop().unwrap().local));

    use super::*;
    use crate::v2ray::ConfigurableV2ray;

    #[tokio::test]
    async fn check_network_timeout_after_v2_startup() -> Result<()> {
        let mut node = get_node_stats()
            .await?
            .first()
            .map(|n| n.0.clone())
            .unwrap();
        node.add.replace("test.add".to_string());

        let prop = get_switch_prop()?;
        let timeout_du = prop.check_timeout + Duration::from_millis(50);

        let v2 = V2.clone();
        let config = v2
            .gen_ping_config(&node, v2.get_available_port().await?)
            .await?;

        let _a = v2.start(&config).await?;

        // client设置的超时正常超时退出
        let res = timeout(
            timeout_du,
            check_networking(
                prop.check_url,
                prop.check_timeout,
                v2.get_proxy_url(&config)?,
            ),
        )
        .await?;
        assert!(res.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn check_network_normal_after_v2_startup() -> Result<()> {
        let node = get_node_stats()
            .await?
            .first()
            .map(|n| n.0.clone())
            .unwrap();
        let v2 = V2.clone();
        let config = v2
            .gen_ping_config(&node, v2.get_available_port().await?)
            .await?;

        let prop = get_switch_prop()?;
        let _a = v2.start(&config).await?;

        check_networking(
            prop.check_url.clone(),
            prop.check_timeout,
            v2.get_proxy_url(&config)?,
        )
        .await?;

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

    #[tokio::test]
    async fn switch_normal_nodes() -> Result<()> {
        let task = get_updated_task().await?;
        switch_node(
            &task.stats,
            task.select_filter.clone(),
            &V2.clone(),
            None,
            None,
        ).await?;
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

    fn get_node() -> Node {
        let link = "vmess://eyJob3N0IjoiaGt0MDEucHFzLXZkcy5sYXkxNjgubmV0IiwicGF0aCI6Ii9obHMiLCJ0bHMiOiIiLCJ2ZXJpZnlfY2VydCI6dHJ1ZSwiYWRkIjoiZ3owMi5tb2JpbGUubGF5MTY4Lm5ldCIsInBvcnQiOjYxMDMzLCJhaWQiOjIsIm5ldCI6IndzIiwiaGVhZGVyVHlwZSI6Im5vbmUiLCJsb2NhbHNlcnZlciI6ImhrdDAxLnBxcy12ZHMubGF5MTY4Lm5ldCIsInYiOiIyIiwidHlwZSI6InZtZXNzIiwicHMiOiLlub/lt54wMuKGkummmea4r0hLVDAxIHwgMS41eCDljp/nlJ8iLCJyZW1hcmsiOiLlub/lt54wMuKGkummmea4r0hLVDAxIHwgMS41eCDljp/nlJ8iLCJpZCI6IjU1ZmIwNDU3LWQ4NzQtMzJjMy04OWEyLTY3OWZlZDZlYWJmMSIsImNsYXNzIjoxfQ==";
        node::parse_node(link).unwrap()
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
retry:
    count: 7
    interval_algo:
        type: "Beb"
        min: "2s"
        max: "40s"
    half:
        start: "02:00:00"
        interval: 5h
        "#;
        serde_yaml::from_str::<SwitchTaskProperty>(content).map_err(Into::into)
    }
}

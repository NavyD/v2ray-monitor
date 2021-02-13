use std::{sync::Arc, time::Duration};

use crate::{
    node::{load_subscription_nodes_from_file, Node},
    task::v2ray_task_config::*,
    v2ray::*,
};
use anyhow::{anyhow, Result};

use super::*;

use regex::Regex;
use reqwest::Proxy;

use tokio::{
    fs::{write, File},
    sync::{Mutex, MutexGuard},
    time::sleep,
};

pub struct V2rayTask {
    node_stats: Arc<Mutex<Vec<(Node, TcpPingStatistic)>>>,
    v2: V2ray,
    property: V2rayTaskProperty,
    cur_node_idx: Arc<Mutex<usize>>,
}

impl V2rayTask {
    pub fn new(property: V2rayTaskProperty) -> Self {
        Self {
            node_stats: Arc::new(Mutex::new(vec![])),
            v2: V2ray::new(property.v2.clone()),
            property,
            cur_node_idx: Arc::new(Mutex::new(0)),
        }
    }

    pub fn with_default() -> Self {
        let property = V2rayTaskProperty::default();
        Self::new(property)
    }

    pub async fn run(&self) -> Result<()> {
        self.load_nodes().await?;
        self.auto_ping();
        self.auto_update_subscription().await?;
        self.auto_swith().await;
        Ok(())
    }

    /// 根据订阅文件自动更新并加载到内存中。
    ///
    async fn auto_update_subscription(&self) -> Result<()> {
        let retry = self.property.subscpt.retry_failed;
        let subscpt = self.property.subscpt.clone();

        let first_interval = {
            let md_interval = File::open(&subscpt.path)
                .await?
                .metadata()
                .await?
                .modified()?
                .elapsed()?;
            log::debug!(
                "{} modified elapsed duration: {:?}",
                subscpt.path,
                md_interval
            );
            subscpt.update_interval.checked_sub(md_interval)
        };

        tokio::spawn(async move {
            if let Some(interval) = first_interval {
                log::info!(
                    "waiting update duration: {:?} from last file {} modified",
                    interval,
                    subscpt.path,
                );
                sleep(interval).await;
            }

            let task = {
                let url = subscpt.url.clone();
                let path = subscpt.path.clone();
                Arc::new(move || update_subscription_owned(url.clone(), path.clone()))
            };
            loop {
                log::debug!("updating subscription");
                match retry_on_owned(
                    task.clone(),
                    next_beb_interval(retry.min_interval, retry.max_interval),
                    retry.count,
                    false,
                )
                .await
                {
                    Ok(retries) => {
                        log::info!(
                            "update subscription task successful on retries: {}",
                            retries
                        );
                    }
                    Err(e) => {
                        log::error!("update subscription task failed: {}", e);
                    }
                };
                log::debug!(
                    "update subscription task sleeping with interval: {:?}",
                    subscpt.update_interval
                );
                sleep(subscpt.update_interval).await;
            }
        });
        Ok(())
    }

    async fn load_nodes(&self) -> Result<()> {
        const EMPTY_PS: TcpPingStatistic = TcpPingStatistic {
            durations: vec![],
            count: 0,
            received_count: 0,
            rtt_avg: None,
            rtt_max: None,
            rtt_min: None,
        };

        let path = &self.property.subscpt.path;
        log::debug!("loading nodes from path: {}", path);
        let name_regex = if let Some(r) = &self.property.tcp_ping.filter.name_regex {
            Some(Regex::new(r)?)
        } else {
            None
        };

        let nodes = load_subscription_nodes_from_file(path)
            .await?
            .into_iter()
            // filter by name regex
            .filter(|node| {
                name_regex.is_none()
                    || name_regex
                        .as_ref()
                        .unwrap()
                        .is_match(node.remark.as_ref().unwrap())
            })
            .map(|n| (n, EMPTY_PS))
            .collect::<Vec<_>>();
        *self.node_stats.lock().await = nodes;
        Ok(())
    }

    async fn auto_swith(&self) {
        let node_stats = self.node_stats.clone();
        let switch = self.property.switch.clone();
        let cur_node_idx = self.cur_node_idx.clone();
        tokio::spawn(async move {
            let task = {
                let check_url = switch.check_url.to_owned();
                let timeout = switch.check_timeout;
                Arc::new(move || check_networking_owned(check_url.clone(), timeout, None))
            };
            let mut all_count = 0;
            let mut last_checked = false;
            let mut max_retries = switch.retry.count;
            loop {
                log::debug!("retrying check networking on all count: {}", all_count);
                match retry_on_owned(
                    task.clone(),
                    next_beb_interval(switch.retry.min_interval, switch.retry.max_interval),
                    max_retries,
                    last_checked,
                )
                .await
                {
                    Ok(retries) => {
                        all_count += retries;
                        if retries <= max_retries && !last_checked {
                            max_retries = std::usize::MAX;
                        }
                        // 上次是失败时 这次是做失败时重试 后成功退出
                        if !last_checked {
                            log::debug!("recovery networking on failed retries: {}", retries);
                        }
                        // 这次做成功时重试 后失败退出
                        else {
                            max_retries = switch.retry.count;
                            log::debug!("found networking problem on success retries: {}", retries);
                        }
                    }
                    // 重试次数达到max_retries
                    Err(e) => {
                        all_count += max_retries;
                        if !last_checked {
                            log::debug!("switching nodes");
                            if let Err(e) = switch_v2ray_ssh(
                                node_stats.clone(),
                                &switch.filter,
                                &switch.ssh,
                                cur_node_idx.clone(),
                            )
                            .await
                            {
                                log::error!("switch v2ray failed: {}", e);
                            }
                            continue;
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
    }

    /// 不断重复在ping_interval间隔中更新。如果失败时将重复直到成功或达到最大重试次数
    fn auto_ping(&self) {
        let stats_lock = self.node_stats.clone();
        let v2 = self.v2.clone();
        let tcp_ping = self.property.tcp_ping.clone();
        let cur_node_idx = self.cur_node_idx.clone();
        tokio::spawn(async move {
            let pp = Arc::new(tcp_ping.ping);
            let retry = tcp_ping.retry_failed;
            let task =
                move || update_tcp_ping_stats_owned(stats_lock.clone(), v2.clone(), pp.clone());
            loop {
                match retry_on(
                    task.clone(),
                    next_beb_interval(retry.min_interval, retry.max_interval),
                    retry.count,
                    false,
                )
                .await
                {
                    Ok(retries) => {
                        *cur_node_idx.lock().await = 0;
                        log::debug!(
                            "auto tcp ping task updated successful on retries: {}. reset cur node idx = 0",
                            retries
                        );
                    }
                    Err(e) => {
                        log::error!("auto tcp ping task failed: {}", e);
                    }
                }
                sleep(tcp_ping.ping_interval).await;
            }
        });
    }
}

async fn switch_v2ray(
    node_stats: Arc<Mutex<Vec<(Node, TcpPingStatistic)>>>,
    selected_size: usize,
    v2: &V2ray,
) -> Result<()> {
    if node_stats
        .lock()
        .await
        .iter()
        .all(|(_, ps)| !ps.is_accessible())
    {
        log::error!("not found any available nodes. please update ping statistics");
        return Err(anyhow!(
            "not found any available nodes. please update ping statistics"
        ));
    }

    // switch on failed again
    let nodes = {
        let nodes = node_stats.lock().await;
        let nodes = if nodes.len() < selected_size {
            &nodes
        } else {
            &nodes[..selected_size]
        };
        nodes
            .iter()
            .map(|v| (v.0.clone(), v.1.clone()))
            .collect::<Vec<_>>()
    };

    if log::log_enabled!(log::Level::Info) {
        let nodes = nodes
            .iter()
            .map(|(n, ps)| (n.remark.as_deref(), ps))
            .collect::<Vec<_>>();
        log::info!("switching with selected nodes: {:?}", nodes);
    }

    let nodes = nodes.iter().map(|v| &v.0).collect::<Vec<_>>();
    v2.restart_load_balance(&nodes).await?;
    Ok(())
}

async fn switch_filter(
    node_stats: Arc<Mutex<Vec<(Node, TcpPingStatistic)>>>,
    filter_prop: &SwitchFilterProperty,
    cur_node_idx: Arc<Mutex<usize>>,
) -> Result<Vec<Node>> {
    let node_stats = node_stats.lock().await;
    if node_stats.iter().all(|(_, ps)| !ps.is_accessible()) {
        return Err(anyhow!(
            "not found any available on {} nodes. please update ping statistics",
            node_stats.len(),
        ));
    }

    // 过滤name
    let nodes = if let Some(re) = filter_prop.name_regex.as_ref() {
        let re = Regex::new(re).expect("regex error");
        node_stats
            .iter()
            .filter(|(node, _)| {
                node.remark.is_some() && re.is_match(node.remark.as_deref().unwrap())
            })
            .collect()
    } else {
        node_stats.iter().collect::<Vec<_>>()
    };
    if nodes.is_empty() {
        return Err(anyhow!(
            "empty nodes is filtered by name regex: {:?}",
            filter_prop.name_regex
        ));
    }

    // 过滤size
    let selected_size = filter_prop.lb_nodes_size as usize;
    if selected_size == 0 {
        return Err(anyhow!("invalid lb_nodes_size: 0"));
    }
    let mut cur_idx = cur_node_idx.lock().await;
    if nodes.len() >= *cur_idx || !nodes[*cur_idx].1.is_accessible() {
        log::warn!("not found available nodes. reset cur idx to 0");
        *cur_idx = 0;
    }
    let mut last_idx = *cur_idx + selected_size;
    if last_idx >= nodes.len() {
        last_idx = nodes.len() - 1;
    }
    log::debug!("selected range {}..={} for nodes", *cur_idx, last_idx);
    let nodes = &nodes[*cur_idx..=last_idx];
    *cur_idx = last_idx + 1;

    // 找出最多相同host的nodes
    let nodes = {
        let mut max_count = 0;
        let mut host = None;
        for (node1, _) in nodes {
            let mut count = 0;
            for (node2, _) in nodes {
                if node1.host == node2.host {
                    count += 1;
                }
            }
            if max_count < count {
                host = node1.host.as_ref();
                max_count = count;
            }
        }
        log::debug!(
            "selected load balance host: {:?}, count: {}",
            host,
            max_count
        );
        nodes
            .iter()
            .filter(|(n, _)| n.host.as_ref() == host)
            .collect::<Vec<_>>()
    };

    if log::log_enabled!(log::Level::Info) {
        let nodes = nodes
            .iter()
            .map(|(n, ps)| (n.remark.as_deref(), ps))
            .collect::<Vec<_>>();
        log::info!("switching with selected nodes: {:?}", nodes);
    }
    let nodes = nodes
        .iter()
        .map(|(node, _)| node.clone())
        .collect::<Vec<_>>();

    Ok(nodes)
}

async fn switch_v2ray_ssh(
    node_stats: Arc<Mutex<Vec<(Node, TcpPingStatistic)>>>,
    filter_prop: &SwitchFilterProperty,
    ssh_prop: &V2raySshProperty,
    cur_node_idx: Arc<Mutex<usize>>,
) -> Result<()> {
    let nodes = switch_filter(node_stats, &filter_prop, cur_node_idx).await?;
    restart_ssh_load_balance(&nodes.iter().collect::<Vec<_>>(), ssh_prop).await?;
    Ok(())
}

async fn check_networking_owned(
    url: String,
    timeout: Duration,
    proxy_url: Option<String>,
) -> Result<()> {
    check_networking(&url, timeout, proxy_url.as_deref()).await
}

async fn check_networking(url: &str, timeout: Duration, proxy_url: Option<&str>) -> Result<()> {
    let mut client = reqwest::Client::builder();
    if let Some(proxy) = proxy_url {
        client = client.proxy(Proxy::all(proxy)?);
    }
    let client = client.timeout(timeout).build()?;
    let status = client.get(url).send().await?.status();
    if !status.is_success() {
        log::info!("switch checking got exception status: {}", status);
    }
    Ok(())
}

async fn update_subscription(url: &str, path: &str) -> Result<()> {
    let contents = reqwest::get(url).await?.bytes().await?;
    log::debug!(
        "writing to {} for subscription contents len: {}",
        path,
        contents.len()
    );
    write(path, contents).await?;
    Ok(())
}

async fn update_subscription_owned(url: String, path: String) -> Result<()> {
    update_subscription(&url, &path).await
}

async fn update_tcp_ping_stats_owned(
    node_stats: Arc<Mutex<Vec<(Node, TcpPingStatistic)>>>,
    v2: V2ray,
    pp: Arc<PingProperty>,
) -> Result<()> {
    update_tcp_ping_stats(node_stats, v2, pp.as_ref()).await
}

/// 执行ping任务后通过tcp ping排序应用到node_stats中
///
/// # Errors
///
/// * 如果node_stats为空
/// * 如果ping后没有任何node是可访问的
///
/// # panic
///
/// * 如果tcp ping后数量与node_stats原始数量对不上
async fn update_tcp_ping_stats(
    node_stats: Arc<Mutex<Vec<(Node, TcpPingStatistic)>>>,
    v2: V2ray,
    pp: &PingProperty,
) -> Result<()> {
    let nodes = {
        let mut stats = node_stats.lock().await;
        if stats.is_empty() {
            log::error!("node stats is empty");
            return Err(anyhow!("node stats is empty, please load nodes"));
        }
        stats.drain(..).map(|v| v.0).collect::<Vec<_>>()
    };
    let old_size = nodes.len();

    let mut stats = v2.tcp_ping_nodes(nodes, &pp).await;

    log::debug!("tcp ping completed with {} size", stats.len());
    if stats.is_empty() {
        panic!("empty nodes after tcp ping");
    } else if stats.len() < old_size {
        log::warn!("tcp ping nodes reduced {}", old_size - stats.len());
    }

    stats.sort_unstable_by(|(_, a), (_, b)| a.cmp(&b));

    if !stats.first().unwrap().1.is_accessible() {
        log::error!("not found any accessible node in {} nodes", stats.len());
        return Err(anyhow!(
            "no any node is accessible: first: {:?}, last: {:?}",
            stats.first().unwrap(),
            stats.last().unwrap()
        ));
    }

    if log::log_enabled!(log::Level::Info) {
        let nodes = stats
            .iter()
            .map(|(n, ps)| (n.remark.as_ref(), ps.rtt_avg.as_ref()))
            .filter(|(_, rtt)| rtt.is_some())
            .collect::<Vec<_>>();
        let mut size = 3;
        if nodes.len() < size {
            size = nodes.len();
        }
        log::info!(
            "auto ping task found accessible {} nodes: {:?}...",
            nodes.len(),
            &nodes[..size]
        );
    }
    *node_stats.lock().await = stats;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn load_nodes_test() -> Result<()> {
        let task = V2rayTask::with_default();
        task.load_nodes().await?;
        let stats = task.node_stats.lock().await;
        assert!(!stats.is_empty());
        assert_eq!(stats.len(), 147);
        Ok(())
    }

    #[tokio::test]
    async fn load_nodes_name_filter() -> Result<()> {
        let mut task = V2rayTask::with_default();
        task.property.tcp_ping.filter = FilterProperty {
            name_regex: Some("安徽→香港01".to_owned()),
        };
        task.load_nodes().await?;
        let stats = task.node_stats.lock().await;
        assert_eq!(stats.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn ping_task_test() -> Result<()> {
        let mut task = V2rayTask::with_default();
        task.property.tcp_ping.filter = FilterProperty {
            name_regex: Some("安徽→香港01".to_owned()),
        };
        task.load_nodes().await?;
        assert_eq!(task.node_stats.lock().await.len(), 1);

        update_tcp_ping_stats(
            task.node_stats.clone(),
            task.v2.clone(),
            &task.property.tcp_ping.ping,
        )
        .await?;

        assert!(task
            .node_stats
            .lock()
            .await
            .iter()
            .any(|(_, ps)| ps.is_accessible()));
        Ok(())
    }

    #[tokio::test]
    async fn ping_task_error_when_unavailable_address() -> Result<()> {
        let mut task = V2rayTask::with_default();
        task.property.tcp_ping.filter = FilterProperty {
            name_regex: Some("安徽→香港01".to_owned()),
        };
        task.load_nodes().await?;

        {
            let mut nodes = task.node_stats.lock().await;
            assert_eq!(nodes.len(), 1);
            nodes[0].0.add = Some("non.host.address".to_owned());
        }

        let res = update_tcp_ping_stats(
            task.node_stats.clone(),
            task.v2.clone(),
            &task.property.tcp_ping.ping,
        )
        .await;
        assert!(res.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn update_subscription_test() -> Result<()> {
        let mut subscpt = SubscriptionProperty::default();
        let start = Instant::now();
        subscpt.path = "/tmp/v2ray-subscription.txt".to_owned();
        update_subscription(&subscpt.url, &subscpt.path).await?;
        let modified_time = File::open(subscpt.path)
            .await?
            .metadata()
            .await?
            .modified()?;
        let elapsed = modified_time.elapsed()?;
        log::debug!("modified time: {:?}, elapsed: {:?}", modified_time, elapsed);
        assert!(elapsed < Instant::now() - start);
        Ok(())
    }

    #[tokio::test]
    async fn switch_v2ray_test() -> Result<()> {
        let mut task = V2rayTask::with_default();
        task.property.tcp_ping.filter = FilterProperty {
            // node_name_regex: Some("安徽→香港01".to_owned()),
            name_regex: Some("香港HKBN01".to_owned()),
        };
        let sp = task.property.switch.clone();
        // task.vp;
        task.load_nodes().await?;

        update_tcp_ping_stats(
            task.node_stats.clone(),
            task.v2.clone(),
            &task.property.tcp_ping.ping,
        )
        .await?;

        switch_v2ray(task.node_stats, 3, &task.v2).await?;
        check_networking(
            &sp.check_url,
            sp.check_timeout,
            Some(&("socks5://127.0.0.1:".to_owned() + &task.property.v2.port.unwrap().to_string())),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn switch_v2ray_ssh_test() -> Result<()> {
        let mut task = V2rayTask::with_default();
        task.property.tcp_ping.filter = FilterProperty {
            // node_name_regex: Some("安徽→香港01".to_owned()),
            name_regex: Some("香港HKBN01".to_owned()),
        };
        let sp = task.property.switch.clone();
        // task.vp;
        task.load_nodes().await?;

        update_tcp_ping_stats(
            task.node_stats.clone(),
            task.v2.clone(),
            &task.property.tcp_ping.ping,
        )
        .await?;
        switch_v2ray_ssh(
            task.node_stats,
            &task.property.switch.filter,
            &task.property.switch.ssh,
            task.cur_node_idx.clone(),
        )
        .await?;

        sleep(Duration::from_millis(1200)).await;
        // check_networking(&sp.check_url, sp.check_timeout, None).await?;
        assert!(*task.cur_node_idx.lock().await > 0);
        Ok(())
    }

    // #[tokio::test]
    // async fn auto_update_subscription() -> Result<()> {
    //     let mut task = V2rayTask::with_default();
    //     task.property.subscpt.update_interval = Duration::from_secs(3);
    //     // mock failure
    //     // task.subspt_property.url = "test.a.b".to_owned();
    //     task.auto_update_subscription().await?;
    //     sleep(Duration::from_secs(10)).await;
    //     Ok(())
    // }

    // #[tokio::test]
    // async fn auto_swith_test() -> Result<()> {
    //     let mut task = V2rayTask::with_default();
    //     task.property.filter = FilterProperty {
    //         // node_name_regex: Some("安徽→香港01".to_owned()),
    //         node_name_regex: Some("粤港03 IEPL专线 入口5".to_owned()),
    //     };
    //     task.load_nodes().await?;

    //     update_tcp_ping_stats(
    //         task.node_stats.clone(),
    //         task.v2.clone(),
    //         &task.property.ping,
    //     )
    //     .await?;
    //     log::debug!("test");
    //     task.auto_swith().await;
    //     sleep(Duration::from_secs(150)).await;
    //     Ok(())
    // }

    // #[tokio::test]
    // async fn auto_ping_task_test() -> Result<()> {
    //     let mut task = V2rayTask::with_default();

    //     task.property.filter.node_name_regex = Some("安徽→香港01".to_owned());
    //     task.property.auto_ping.ping_interval = Duration::from_secs(1);
    //     task.load_nodes().await?;

    //     {
    //         let mut nodes = task.node_stats.lock().await;
    //         assert_eq!(nodes.len(), 1);
    //         nodes[0].0.add = Some("non.host.address".to_owned());
    //     }

    //     task.auto_ping();
    //     sleep(Duration::from_secs(30)).await;
    //     assert!(task
    //         .node_stats
    //         .lock()
    //         .await
    //         .iter()
    //         .any(|(_, ps)| ps.is_accessible()));
    //     Ok(())
    // }
}

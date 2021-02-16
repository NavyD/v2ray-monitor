use std::{cmp::Ordering, collections::BinaryHeap, sync::Arc, time::Duration};

use crate::{
    tcp_ping::TcpPingStatistic,
    v2ray::{node::Node, V2rayService},
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;

use parking_lot::Mutex;
use reqwest::Proxy;

use super::{
    filter::{Filter, *},
    v2ray_task_config::{SwitchFilterProperty, SwitchTaskProperty},
    RetryService, TaskRunnable,
};

#[derive(Clone)]
pub struct SwitchNodeStat {
    pub node: Node,
    pub tcp_stat: TcpPingStatistic,
    pub serv_duras: Vec<Duration>,
}

impl SwitchNodeStat {
    pub fn new(node: Node, ps: TcpPingStatistic) -> Self {
        Self {
            node,
            tcp_stat: ps,
            serv_duras: vec![],
        }
    }

    pub fn push_serv_duration(&mut self, d: Duration) {
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
        let r = self.weight().cmp(&other.weight());
        if r != Ordering::Equal {
            return r;
        }
        if self.node == other.node {
            return Ordering::Equal;
        }

        let compare = |a: Option<Duration>, b: Option<Duration>| match (a, b) {
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (a, b) => a.cmp(&b),
        };
        let v = compare(self.tcp_stat.rtt_avg, other.tcp_stat.rtt_avg);
        if v != Ordering::Equal {
            return v;
        }
        let v = compare(self.tcp_stat.rtt_min, other.tcp_stat.rtt_min);
        if v != Ordering::Equal {
            return v;
        }
        compare(self.tcp_stat.rtt_max, other.tcp_stat.rtt_max)
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
    pre_filter: Box<dyn Filter<SwitchData, ()>>,
    select_filter: Box<dyn Filter<SwitchData, Vec<SwitchNodeStat>>>,
    prop: SwitchTaskProperty,
}

impl<V: V2rayService> SwitchTask<V> {
    pub fn new(prop: SwitchTaskProperty, v2: V) -> Self {
        Self {
            stats: Arc::new(Mutex::new(BinaryHeap::new())),
            v2,
            pre_filter: Box::new(NameRegexFilter::new(&[prop
                .filter
                .name_regex
                .clone()
                .unwrap()])),
            select_filter: Box::new(SwitchSelectFilter::new(prop.filter.lb_nodes_size.into())),
            prop,
        }
    }

    pub async fn update_node_stats(&self, node_stats: Vec<(Node, TcpPingStatistic)>) {
        *self.stats.lock() = node_stats
            .into_iter()
            .map(|(n, ps)| SwitchNodeStat::new(n, ps))
            .collect::<BinaryHeap<_>>();
        self.pre_filter.filter(self.stats.clone());
    }
}

#[async_trait]
impl<V: V2rayService> TaskRunnable for SwitchTask<V> {
    async fn run(&self) -> anyhow::Result<()> {
        if self.stats.lock().is_empty() {
            return Err(anyhow!("no any stats"));
        }
        let switch = self.prop.clone();
        let task = {
            let check_url = switch.check_url.to_owned();
            let timeout = switch.check_timeout;
            move || check_networking_owned(check_url.clone(), timeout, None)
        };
        let mut all_count = 0;
        let mut last_checked = false;
        let mut max_retries = switch.retry.count;
        let (mut last_switched, mut last_dura) = (None::<Vec<SwitchNodeStat>>, None);
        let retry_srv = RetryService::new(switch.retry.clone(), task);
        loop {
            log::debug!("retrying check networking on all count: {}", all_count);
            match retry_srv.retry_on(last_checked).await {
                Ok((retries, duration)) => {
                    all_count += retries;
                    // 失败重试时退出表示 成功  使用最大值重试
                    if retries <= max_retries && !last_checked {
                        max_retries = std::usize::MAX;
                        log::debug!(
                            "reset max_retries as max because last_checked: {}, retries: {}",
                            last_checked,
                            retries
                        );
                    }
                    // 上次是失败时 这次是做失败时重试 后成功退出
                    if !last_checked {
                        log::debug!("recovery networking on failed retries: {}", retries);
                    }
                    // 这次做成功时重试 后失败退出
                    else {
                        last_dura = Some(duration);
                        max_retries = switch.retry.count;
                        log::debug!("found networking problem on success retries: {}", retries);
                    }
                }
                // 重试次数达到max_retries
                Err(e) => {
                    all_count += max_retries;
                    if !last_checked {
                        log::debug!("selecting for switching nodes");
                        // 从stats中取出节点
                        let selected = self.select_filter.filter(self.stats.clone());
                        if log::log_enabled!(log::Level::Info) {
                            let nodes_msg = selected
                                .iter()
                                .map(|v| (v.node.remark.as_ref(), v.tcp_stat.rtt_avg.as_ref()))
                                .collect::<Vec<_>>();
                            log::info!("selected nodes: {:?}", nodes_msg);
                        }

                        // 将上次切换的节点重入 stats 权重排序
                        if let Some(last) = last_switched {
                            log::debug!("repush for last switched nodes size: {}", last.len());
                            let mut stats = self.stats.lock();
                            for mut stat in last {
                                log::trace!("repush node {:?}, old service duration: {:?}, cur switch duration: {:?}", stat.node.remark, stat.serv_duras, last_dura);
                                stat.push_serv_duration(last_dura.unwrap());
                                stats.push(stat);
                            }
                        }
                        // 切换节点
                        let config = self
                            .v2
                            .gen_config(
                                &selected
                                    .iter()
                                    .map(|s| s.node.clone())
                                    .collect::<Vec<_>>()
                                    .iter()
                                    .collect::<Vec<_>>(),
                            )
                            .await?;
                        self.v2.start_in_background(&config).await?;

                        last_switched = Some(selected);

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
    }
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::{
        task::{find_v2ray_bin_path, v2ray_task_config::LocalV2rayProperty},
        v2ray::LocalV2ray,
    };

    use super::*;

    #[tokio::test]
    async fn basic() -> Result<()> {
        let prop = get_switch_prop()?;
        let v2 = LocalV2ray::new(prop.local.clone());
        let task = SwitchTask::new(prop, v2);

        task.run().await?;
        Ok(())
    }

    fn get_local_prop() -> Result<LocalV2rayProperty> {
        Ok(LocalV2rayProperty {
            bin_path: find_v2ray_bin_path()?,
            config_path: Some("tests/data/local-v2-config.json".to_string()),
        })
    }

    fn get_switch_prop() -> Result<SwitchTaskProperty> {
        let content = r#"
check_url: https://www.google.com/gen_204
check_timeout: 2s
filter:
    lb_nodes_size: 3
    name_regex: "→香港"
retry:
    count: 7
    interval_algo:
        type: "Beb"
        min: "2s"
        max: "40s"
    half:
        start: "02:00:00"
        interval: 5h
ssh:
    username: root
    host: 192.168.93.2
    config_path: /var/etc/ssrplus/tcp-only-ssr-retcp.json
    bin_path: /usr/bin/v2ray
local:
    config_path: tests/data/local-v2-config.json
    
        "#;
        serde_yaml::from_str::<SwitchTaskProperty>(content).map_err(Into::into)
    }
}

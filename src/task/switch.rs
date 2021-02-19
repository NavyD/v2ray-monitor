use std::{
    cmp::Ordering,
    collections::BinaryHeap,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    tcp_ping::TcpPingStatistic,
    v2ray::{node::Node, V2rayService},
};
use anyhow::{anyhow, Result};

use parking_lot::Mutex;
use reqwest::Proxy;
use tokio::sync::mpsc::Receiver;

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

pub struct SwitchTask<V: V2rayService> {
    stats: SwitchData,
    v2: V,
    pre_filter: Option<Arc<NameRegexFilter>>,
    select_filter: Box<dyn Filter<SwitchData, Vec<SwitchNodeStat>>>,
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
            select_filter: Box::new(SwitchSelectFilter::new(prop.filter.lb_nodes_size.into())),
            prop,
        }
    }

    pub async fn run(&self, rx: Receiver<Vec<(Node, TcpPingStatistic)>>) -> Result<()> {
        self.update_node_stats(rx).await?;
        let retry_srv = RetryService::new(self.prop.retry.clone());

        let switch = self.prop.clone();
        self.v2.clean_env().await?;

        let task = {
            let proxy_url = self.v2.get_proxy_url(self.v2.get_config().await?)?;
            let check_url = switch.check_url.clone();
            let timeout = switch.check_timeout;
            Arc::new(move || check_networking(check_url.clone(), timeout, proxy_url.clone()))
        };
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
                        let selected = self
                            .switch(last_switched.take(), last_duration.take())
                            .await?;
                        log::debug!("Node switch succeeded: {:?}", selected);
                        last_switched.replace(selected);
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

    async fn switch(
        &self,
        last_switched: Option<Vec<SwitchNodeStat>>,
        last_dura: Option<Duration>,
    ) -> Result<Vec<SwitchNodeStat>> {
        // 将上次切换的节点重入 stats 权重排序
        if let Some(last) = last_switched {
            log::debug!("repush for last switched nodes size: {}", last.len());
            let mut stats = self.stats.lock();
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
        let selected = self.select_filter.filter(self.stats.clone());

        if log::log_enabled!(log::Level::Info) {
            let nodes_msg = selected
                .iter()
                .map(|v| (v.node.remark.as_ref(), v.tcp_stat.rtt_avg.as_ref()))
                .collect::<Vec<_>>();
            log::info!("selected nodes: {:?}", nodes_msg);
        }

        if selected.is_empty() {
            log::error!("Switch node error: no any nodes");
            return Err(anyhow!("no any nodes"));
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

        self.v2.restart_in_background(&config).await?;

        Ok(selected)
    }
}

async fn check_networking(url: String, timeout: Duration, proxy_url: Option<String>) -> Result<()> {
    let client = proxy_url
        .as_ref()
        .map(|url| Proxy::all(url).map(|proxy| reqwest::Client::builder().proxy(proxy)))
        .unwrap_or_else(|| Ok(reqwest::Client::builder()))?
        .timeout(timeout)
        .build()?;

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

// #[cfg(test)]
// mod tests {

//     use crate::{
//         task::{
//             find_v2ray_bin_path,
//             v2ray_task_config::{LocalV2rayProperty, V2rayProperty},
//         },
//         v2ray::LocalV2ray,
//     };

//     use super::*;

//     #[tokio::test]
//     async fn basic() -> Result<()> {
//         let prop = get_switch_prop()?;
//         let v2 = LocalV2ray::new(get_v2ray_prop()?.local);
//         let _task = SwitchTask::new(prop, v2);

//         // task.run().await?;
//         Ok(())
//     }

//     fn get_v2ray_prop() -> Result<V2rayProperty> {
//         let content = r#"
// ssh:
//     username: root
//     host: 192.168.93.2
//     config_path: /var/etc/ssrplus/tcp-only-ssr-retcp.json
//     bin_path: /usr/bin/v2ray
// local:
//     config_path: tests/data/local-v2-config.json
//         "#;
//         serde_yaml::from_str::<V2rayProperty>(content).map_err(Into::into)
//     }

//     fn get_switch_prop() -> Result<SwitchTaskProperty> {
//         let content = r#"
// check_url: https://www.google.com/gen_204
// check_timeout: 2s
// filter:
//     lb_nodes_size: 3
//     name_regex: "→香港"
// retry:
//     count: 7
//     interval_algo:
//         type: "Beb"
//         min: "2s"
//         max: "40s"
//     half:
//         start: "02:00:00"
//         interval: 5h
//         "#;
//         serde_yaml::from_str::<SwitchTaskProperty>(content).map_err(Into::into)
//     }
// }

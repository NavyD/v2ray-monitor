use std::{
    collections::HashMap,
    convert::TryInto,
    sync::Arc,
    time::{Duration, SystemTime},
};

use crate::{
    node::{load_subscription_nodes_from_file, Node},
    v2ray::*,
};
use anyhow::{anyhow, Result};
use atime::sleep;
use futures::Future;
use tokio::{
    fs::{write, File, OpenOptions},
    sync::Mutex,
    time as atime,
};
trait NodeFilter {
    fn filter(nodes: Vec<Node>) -> Vec<Node>;
}

#[derive(Debug)]
struct FilterProperty {
    node_name_regex: Option<String>,
    max_tcp_ping: Option<Duration>,
    sort_limit: Option<usize>,
}

struct SubscriptionProperty {
    subscription_path: String,
    update_subscription_interval: Duration,
    url: String,
}

struct V2rayTaskProperty {
    ping_interval: Duration,
}

struct SwitchProperty {
    check_url: String,
    check_timeout: Duration,
}

struct V2rayTask {
    node_stats: Arc<Mutex<Vec<(Node, TcpPingStatistic)>>>,
    property: V2rayTaskProperty,
    subspt_property: SubscriptionProperty,
    switch_property: SwitchProperty,
    vp: Arc<V2rayProperty>,
    pp: Arc<PingProperty>,
    fp: Arc<FilterProperty>,
}

impl V2rayTask {
    /// 根据订阅文件自动更新并加载到内存中。
    ///
    pub async fn auto_update_subscription(&mut self) -> Result<()> {
        let path = &self.subspt_property.subscription_path;
        let md_interval = File::open(path)
            .await?
            .metadata()
            .await?
            .modified()?
            .elapsed()?;

        let interval = self.subspt_property.update_subscription_interval;
        let first_interval = interval.checked_sub(md_interval);
        let url = self.subspt_property.url.clone();
        let path = path.clone();

        tokio::spawn(async move {
            if let Some(interval) = first_interval {
                log::debug!(
                    "waiting update duration: {:?} for last file {} modified elapsed duration {:?}",
                    interval,
                    path,
                    md_interval
                );
                sleep(interval).await;
            }
            let mut interval = atime::interval(interval);
            let mut count = 0;
            loop {
                count += 1;
                interval.tick().await;

                if let Err(e) = update_subscription(&url, &path).await {
                    log::warn!(
                        "{}th update subscription url: {}, path: {}, error: {}",
                        count,
                        url,
                        path,
                        e
                    );
                } else {
                    log::info!("{}th update subscription successful", count);
                }
            }
        });
        Ok(())
    }

    async fn load_nodes(&mut self) -> Result<()> {
        const EMPTY_PS: TcpPingStatistic = TcpPingStatistic {
            durations: vec![],
            count: 0,
            received_count: 0,
            rtt_avg: None,
            rtt_max: None,
            rtt_min: None,
        };
        let path = &self.subspt_property.subscription_path;
        let nodes = load_subscription_nodes_from_file(path)
            .await?
            .into_iter()
            .map(|n| (n, EMPTY_PS))
            .collect::<Vec<_>>();
        *self.node_stats.lock().await = nodes;
        Ok(())
    }

    pub async fn auto_swith(&self) -> Result<()> {
        // 0. check networking status
        async fn check(url: &str, timeout: Duration) -> Result<()> {
            let client = reqwest::Client::builder().timeout(timeout).build()?;
            let status = client.get(url).send().await?.status();
            if !status.is_success() {
                log::info!("switch checking got exception status: {}", status);
            }
            Ok(())
        }
        // 1. 假设开始成功 时继续到最大间隔重试

        // 2. 在成功出现失败时，先重试一定次数，再开始切换
        todo!()
    }

    pub async fn auto_ping(&mut self) -> Result<()> {
        let interval = self.property.ping_interval;

        let vp = self.vp.clone();
        let pp = self.pp.clone();
        let fp = self.fp.clone();
        let stats_lock = self.node_stats.clone();

        tokio::spawn(async move {
            let mut interval = atime::interval(interval);
            log::debug!("starting auto ping for interval: {:?}", interval);
            let mut count = 0;
            loop {
                interval.tick().await;

                count += 1;
                log::debug!("auto ping {}th", count);

                let nodes = {
                    let mut stats = stats_lock.lock().await;
                    if stats.is_empty() {
                        log::warn!("node stats is empty, please load nodes");
                        continue;
                    }
                    stats.drain(..).map(|v| v.0).collect::<Vec<_>>()
                };

                let stats = tcp_ping_nodes(nodes, &vp, &pp).await;
                log::debug!("tcp ping completed with {} size", stats.len());
                if stats.is_empty() {
                    log::warn!("not found any ping stats");
                    continue;
                }

                log::debug!("filtering with stats");
                let stats = filter_stats(stats, &fp);
                if stats.is_empty() {
                    log::warn!("all nodes filterd for property: {:?}", fp);
                } else {
                    log::info!("auto tcp ping completed: {} size", stats.len());
                    *stats_lock.lock().await = stats;
                }
            }
        });
        Ok(())
    }
}

async fn v2ray_switch() -> Result<()> {
    
    Ok(())
}

fn filter_stats(
    node_stats: Vec<(Node, TcpPingStatistic)>,
    fp: &FilterProperty,
) -> Vec<(Node, TcpPingStatistic)> {
    let mut stats = node_stats
        .into_iter()
        .filter(|(_, ps)| ps.rtt_avg.is_some() || ps.rtt_avg <= fp.max_tcp_ping)
        .collect::<Vec<_>>();
    stats.sort_unstable_by_key(|k| k.1.rtt_avg);
    stats
}

async fn update_subscription(url: &str, path: &str) -> Result<()> {
    let contents = reqwest::get(url).await?.bytes().await?;
    write(path, contents).await?;
    Ok(())
}

async fn retry_on<T, F, Fut>(
    f: F,
    next_itv: impl Fn(Duration) -> Duration,
    min_itv: Duration,
    max_itv: Duration,
    succ_or_fail: bool,
) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    f().await?;
    Ok(())
}

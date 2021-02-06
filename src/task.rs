use std::{
    collections::HashMap,
    convert::TryInto,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use crate::{
    node::{load_subscription_nodes_from_file, Node},
    v2ray::*,
};
use anyhow::{anyhow, Context, Result};
use atime::sleep;
use futures::Future;
use log::warn;
use once_cell::sync::Lazy;
use regex::Regex;
use tokio::{
    fs::{write, File},
    process::Child,
    sync::Mutex,
    time::{self as atime},
};
trait NodeFilter {
    fn filter(nodes: Vec<Node>) -> Vec<Node>;
}

#[derive(Debug)]
struct FilterProperty {
    node_name_regex: Option<String>,
}

impl Default for FilterProperty {
    fn default() -> Self {
        Self {
            node_name_regex: None,
        }
    }
}

pub struct SubscriptionProperty {
    path: String,
    update_interval: Duration,
    url: String,
}

impl Default for SubscriptionProperty {
    fn default() -> Self {
        let cur_dir = std::env::current_dir()
            .ok()
            .and_then(|d| d.to_str().map(|s| s.to_owned()))
            .unwrap();
        Self {
            path: cur_dir,
            update_interval: Duration::from_secs(60 * 60 * 12),
            url: "https://www.jinkela.site/link/ukWr5K49YjHIQGdL?sub=3".to_owned(),
        }
    }
}

struct PingTaskProperty {
    ping_interval: Duration,
}

impl Default for PingTaskProperty {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(60),
        }
    }
}

struct SwitchProperty {
    check_url: String,
    check_timeout: Duration,
    interval: Duration,

    nodes_size: u8,

    max_retries_failed: usize,
    max_retry_interval: Duration,
    min_retry_interval: Duration,
}

impl Default for SwitchProperty {
    fn default() -> Self {
        Self {
            check_url: "https://www.google.com/gen_204".to_owned(),
            check_timeout: Duration::from_secs(3),
            interval: Duration::from_secs(2),
            nodes_size: 3,
            max_retries_failed: 6,
            max_retry_interval: Duration::from_secs(10),
            min_retry_interval: Duration::from_secs(1),
        }
    }
}

static V2_CHILD: Lazy<Mutex<Option<Child>>> = Lazy::new(|| Mutex::new(None));

pub struct V2rayTask {
    node_stats: Arc<Mutex<Vec<(Node, TcpPingStatistic)>>>,
    ptp: PingTaskProperty,
    subspt_property: SubscriptionProperty,
    switch_property: SwitchProperty,
    vp: Arc<V2rayProperty>,
    pp: Arc<PingProperty>,
    fp: Arc<FilterProperty>,
}

impl V2rayTask {
    pub fn with_default() -> Self {
        Self {
            node_stats: Arc::new(Mutex::new(vec![])),
            ptp: PingTaskProperty::default(),
            subspt_property: SubscriptionProperty::default(),
            switch_property: SwitchProperty::default(),
            vp: Arc::new(V2rayProperty::default()),
            pp: Arc::new(PingProperty::default()),
            fp: Arc::new(FilterProperty::default()),
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        self.load_nodes().await?;
        self.auto_ping().await?;
        self.auto_update_subscription().await?;
        self.auto_swith().await
    }
    /// 根据订阅文件自动更新并加载到内存中。
    ///
    async fn auto_update_subscription(&mut self) -> Result<()> {
        let path = &self.subspt_property.path;
        let md_interval = File::open(path)
            .await?
            .metadata()
            .await?
            .modified()?
            .elapsed()?;

        let interval = self.subspt_property.update_interval;
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
        let name_regex = if let Some(r) = &self.fp.node_name_regex {
            Some(Regex::new(r)?)
        } else {
            None
        };
        let path = &self.subspt_property.path;
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

    async fn auto_swith(&mut self) -> Result<()> {
        // 0. check networking status
        async fn check(url: &str, timeout: Duration) -> Result<()> {
            let client = reqwest::Client::builder().timeout(timeout).build()?;
            let status = client.get(url).send().await?.status();
            if !status.is_success() {
                log::info!("switch checking got exception status: {}", status);
            }
            Ok(())
        }

        let timeout = self.switch_property.check_timeout;
        let max_retries = self.switch_property.max_retries_failed;
        let (min_itv, max_itv) = (
            self.switch_property.min_retry_interval,
            self.switch_property.max_retry_interval,
        );
        let selected_size = self.switch_property.nodes_size as usize;
        let next = move |last: Duration, retries: usize| {
            if retries == 0 {
                min_itv
            } else if last > max_itv {
                max_itv
            } else {
                last + last
            }
        };
        let last_checked = Arc::new(Mutex::new(false));

        let mut interval = atime::interval(self.switch_property.interval);
        loop {
            interval.tick().await;

            {
                if let Err(e) = last_checked.try_lock() {
                    log::debug!("auto switch is running: {}", e);
                    continue;
                }
            }

            let url = self.switch_property.check_url.clone();
            let vp = self.vp.clone();
            let last_checked = last_checked.clone();
            let nodes = self.node_stats.lock().await;
            let nodes = nodes
                .iter()
                .filter(|(_, ps)| ps.is_accessible())
                .map(|(node, _)| node.clone())
                .collect::<Vec<_>>();
            if nodes.is_empty() {
                log::info!("not found any available nodes. please update ping statistics");
                continue;
            }
            tokio::spawn(async move {
                let mut last_checked = last_checked.lock().await;
                let max_retries = if *last_checked {
                    std::usize::MAX
                } else {
                    max_retries
                };
                // 1. 假设开始失败 时继续到最大间隔重试
                let retries =
                    retry_on(|| check(&url, timeout), next, max_retries, *last_checked).await;
                // swith for failed
                if *last_checked || retries >= max_retries {
                    let nodes = &nodes.iter().collect::<Vec<_>>()[..selected_size];
                    let child = V2_CHILD.lock().await.take();
                    match restart_load_balance(&vp, nodes, child).await {
                        // 保留v2ray进程
                        Ok(child) => {
                            V2_CHILD.lock().await.replace(child);
                        }
                        Err(e) => {
                            log::warn!("switch v2ray error: {}", e);
                        }
                    }
                }
                // 2. 在成功出现失败时，先重试一定次数，再开始切换
                *last_checked = !*last_checked;
            });
        }
    }

    async fn auto_ping(&mut self) -> Result<()> {
        let interval = self.ptp.ping_interval;

        let vp = self.vp.clone();
        let pp = self.pp.clone();
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

                let mut stats = tcp_ping_nodes(nodes, &vp, &pp).await;
                log::debug!("tcp ping completed with {} size", stats.len());
                if stats.is_empty() {
                    log::warn!("not found any nodes for tcp ping");
                } else {
                    stats.sort_unstable_by(|(_, a), (_, b)| a.cmp(&b));
                    if !stats[0].1.is_accessible() {
                        log::warn!(
                            "no any node is accessible: first: {:?}, last: {:?}",
                            stats.first().unwrap(),
                            stats.last().unwrap()
                        );
                    } else {
                        *stats_lock.lock().await = stats;
                    }
                }
            }
        });
        Ok(())
    }
}

async fn update_subscription(url: &str, path: &str) -> Result<()> {
    let contents = reqwest::get(url).await?.bytes().await?;
    write(path, contents).await?;
    Ok(())
}

async fn retry_on<F, Fut>(
    func: F,
    next_itv_func: impl Fn(Duration, usize) -> Duration,
    max_retries: usize,
    succ_or_fail: bool,
) -> usize
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let retried = |r: Result<()>| (succ_or_fail && r.is_ok()) || (!succ_or_fail && r.is_err());
    let mut last_itv = Duration::from_nanos(0);
    let mut retry_count = 0;
    let start = Instant::now();
    while retried(func().await) && retry_count <= max_retries {
        retry_count += 1;
        last_itv = next_itv_func(last_itv, retry_count);
        sleep(last_itv).await;
    }
    let d = Instant::now() - start;
    log::debug!(
        "{} exit after {} retries, it takes {:?}",
        if succ_or_fail { "success" } else { "failed" },
        retry_count,
        d
    );
    retry_count
}

// async fn retry_on_fail<F, Fut>(func: F, next_itv_func: impl Fn(Duration, usize) -> Duration)
// where
//     F: Fn() -> Fut,
//     Fut: Future<Output = Result<()>>,
// {
// }

// async fn retry_on_success<F, Fut>(func: F, next_itv_func: impl Fn(Duration, usize) -> Duration)
// where
//     F: Fn() -> Fut,
//     Fut: Future<Output = Result<()>>,
// {
//     let mut retry_count = 0;
//     let mut last_itv = Duration::from_nanos(0);
//     let start = Instant::now();
//     while func().await.is_ok() {
//         retry_count += 1;
//         last_itv = next_itv_func(last_itv, retry_count);
//         sleep(last_itv).await;
//     }
//     let d = Instant::now() - start;
//     log::debug!("eixt after {} retries, it takes {:?}", retry_count, d);
// }

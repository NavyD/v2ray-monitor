use std::{
    fmt::{Debug, Display},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    node::{load_subscription_nodes_from_file, Node},
    v2ray::*,
};
use anyhow::{anyhow, Result};
use atime::sleep;
use futures::Future;

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

#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct SubscriptionProperty {
    path: String,
    update_interval: Duration,
    url: String,

    max_retries_failed: usize,
    min_retry_interval: Duration,
    max_retry_interval: Duration,
}

impl Default for SubscriptionProperty {
    fn default() -> Self {
        let cur_subspt_path = std::env::current_dir()
            .map(|d| d.join("v2ray-subscription.txt"))
            .ok()
            .and_then(|d| d.to_str().map(|s| s.to_owned()))
            .unwrap();
        Self {
            path: cur_subspt_path,
            update_interval: Duration::from_secs(60 * 60 * 12),
            url: "https://www.jinkela.site/link/ukWr5K49YjHIQGdL?sub=3".to_owned(),

            max_retries_failed: 3,
            max_retry_interval: Duration::from_secs(60),
            min_retry_interval: Duration::from_secs(2),
        }
    }
}

struct PingTaskProperty {
    ping_interval: Duration,
    max_retries_failed: usize,
    min_retry_interval: Duration,
    max_retry_interval: Duration,
}

impl Default for PingTaskProperty {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(60 * 10),
            max_retries_failed: 3,
            max_retry_interval: Duration::from_secs(60),
            min_retry_interval: Duration::from_secs(2),
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
    vp: V2rayProperty,
    pp: PingProperty,
    fp: FilterProperty,
}

impl V2rayTask {
    pub fn with_default() -> Self {
        Self {
            node_stats: Arc::new(Mutex::new(vec![])),
            ptp: PingTaskProperty::default(),
            subspt_property: SubscriptionProperty::default(),
            switch_property: SwitchProperty::default(),
            vp: V2rayProperty::default(),
            pp: PingProperty::default(),
            fp: FilterProperty::default(),
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        self.load_nodes().await?;
        self.auto_ping();
        self.auto_update_subscription().await?;
        self.auto_swith().await
    }

    /// 根据订阅文件自动更新并加载到内存中。
    ///
    async fn auto_update_subscription(&mut self) -> Result<()> {
        let url = self.subspt_property.url.clone();
        let path = self.subspt_property.path.clone();
        let interval = self.subspt_property.update_interval;
        let max_retries_failed = self.subspt_property.max_retries_failed;
        let (min_itv, max_itv) = (
            self.subspt_property.min_retry_interval,
            self.subspt_property.max_retry_interval,
        );

        let first_interval = {
            let md_interval = File::open(&path)
                .await?
                .metadata()
                .await?
                .modified()?
                .elapsed()?;
            log::debug!("{} modified elapsed duration: {:?}", path, md_interval);
            interval.checked_sub(md_interval)
        };

        tokio::spawn(async move {
            if let Some(interval) = first_interval {
                log::debug!(
                    "waiting update duration: {:?} from last file {} modified",
                    interval,
                    path,
                );
                sleep(interval).await;
            }

            let task = Arc::new(move || update_subscription_owned(url.clone(), path.clone()));
            loop_with_interval(
                move || {
                    retry_on_owned(
                        task.clone(),
                        next_beb_interval(min_itv, max_itv),
                        max_retries_failed,
                        false,
                    )
                },
                interval,
                max_itv,
            )
            .await;
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

        let path = &self.subspt_property.path;
        log::debug!("loading nodes from path: {}", path);
        let name_regex = if let Some(r) = &self.fp.node_name_regex {
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
                match retry_on(|| check(&url, timeout), next, max_retries, *last_checked).await {
                    Ok(retries) => {
                        log::debug!("retries: {}", retries);
                    }
                    Err(e) => {
                        if !*last_checked {
                            log::info!("check multiple failures and start automatic switching");
                            // switch on failed again
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
                        } else {
                            log::debug!(
                                "Check that a failure has occurred: {}, try again if it fails",
                                e
                            );
                        }
                    }
                }

                *last_checked = !*last_checked;
            });
        }
    }

    /// 成功ping时使用默认的interval，如果失败则使用`PingTaskProperty::max_retry_interval`不断重试
    fn auto_ping(&mut self) {
        let ping_interval = self.ptp.ping_interval;
        let vp = Arc::new(self.vp.clone());
        let pp = Arc::new(self.pp.clone());
        let stats_lock = self.node_stats.clone();
        let max_retries = self.ptp.max_retries_failed;
        let (min_itv, max_itv) = (self.ptp.min_retry_interval, self.ptp.max_retry_interval);

        tokio::spawn(async move {
            loop_with_interval(
                move || {
                    let vp = vp.clone();
                    let pp = pp.clone();
                    let stats_lock = stats_lock.clone();
                    let task = move || update_tcp_ping_stats_owned(stats_lock.clone(), vp.clone(), pp.clone());
                    retry_on(
                        task,
                        next_beb_interval(min_itv, max_itv),
                        max_retries,
                        false,
                    )
                },
                ping_interval,
                max_itv,
            )
            .await;
        });
    }
}

fn next_beb_interval(min_itv: Duration, max_itv: Duration) -> impl Fn(Duration, usize) -> Duration {
    move |last, _retries| {
        if last < min_itv {
            min_itv
        } else if last > max_itv {
            max_itv
        } else {
            last + last
        }
    }
}

/// 如果func执行成功则使用interval sleep，否则使用max_retry_interval
async fn loop_with_interval<F, Fut, T>(
    func: F,
    interval: Duration,
    max_retry_interval: Duration,
) -> !
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>> + Send + 'static,
    T: Debug + Display,
{
    loop {
        let interval = match func().await {
            Ok(retries) => {
                log::debug!("loop successful on retries: {}", retries);
                interval
            }
            Err(e) => {
                log::warn!("use max retry interval {:?} for loop failed: {}", max_retry_interval, e);
                max_retry_interval
            }
        };
        log::debug!("loop sleeping with interval: {:?}", interval);
        sleep(interval).await;
    }
}

async fn update_subscription(url: &str, path: &str) -> Result<()> {
    let contents = reqwest::get(url).await?.bytes().await?;
    log::debug!("writing to {} for subscription contents len: {}", path, contents.len());
    write(path, contents).await?;
    Ok(())
}

async fn update_subscription_owned(url: String, path: String) -> Result<()> {
    update_subscription(&url, &path).await
}

async fn update_tcp_ping_stats_owned(
    node_stats: Arc<Mutex<Vec<(Node, TcpPingStatistic)>>>,
    vp: Arc<V2rayProperty>,
    pp: Arc<PingProperty>,
) -> Result<()> {
    update_tcp_ping_stats(node_stats, vp.as_ref(), pp.as_ref()).await
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
    vp: &V2rayProperty,
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

    let mut stats = tcp_ping_nodes(nodes, &vp, &pp).await;
    log::debug!("tcp ping completed with {} size", stats.len());
    if stats.len() != old_size {
        panic!(
            "wrong node len: {} after tcp ping len: {}",
            stats.len(),
            old_size
        );
    }

    stats.sort_unstable_by(|(_, a), (_, b)| a.cmp(&b));

    if !stats[0].1.is_accessible() {
        return Err(anyhow!(
            "no any node is accessible: first: {:?}, last: {:?}",
            stats.first().unwrap(),
            stats.last().unwrap()
        ));
    }

    *node_stats.lock().await = stats;
    Ok(())
}

async fn retry_on_owned<F, Fut>(
    func: Arc<F>,
    next_itv_func: impl Fn(Duration, usize) -> Duration,
    max_retries: usize,
    succ_or_fail: bool,
) -> Result<usize>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<()>>,
{
    retry_on(func.as_ref(), next_itv_func, max_retries, succ_or_fail).await
}

async fn retry_on<F, Fut>(
    func: F,
    next_itv_func: impl Fn(Duration, usize) -> Duration,
    max_retries: usize,
    succ_or_fail: bool,
) -> Result<usize>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let retried = |r: Result<()>| {
        if let Err(e) = &r {
            log::trace!("there was an error retrying: {}", e);
        }
        (succ_or_fail && r.is_ok()) || (!succ_or_fail && r.is_err())
    };
    let name = if !succ_or_fail { "success" } else { "failed" };
    let mut last_itv = Duration::from_nanos(0);
    let mut retry_count = 0;
    let start = Instant::now();
    while retried(func().await) {
        if retry_count == std::usize::MAX {
            panic!("overflow usize retry_count");
        }
        if retry_count > max_retries {
            return Err(anyhow!(
                "retrying on {} reaches max retries {} exit",
                name,
                max_retries
            ));
        }
        retry_count += 1;
        last_itv = next_itv_func(last_itv, retry_count);
        log::debug!("sleeping {:?} on retries: {}", last_itv, retry_count);
        sleep(last_itv).await;
    }
    let d = Instant::now() - start;
    log::debug!(
        "{} exit after {} retries, it takes {:?}",
        name,
        retry_count,
        d
    );
    Ok(retry_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn load_nodes_test() -> Result<()> {
        let mut task = V2rayTask::with_default();
        task.load_nodes().await?;
        let stats = task.node_stats.lock().await;
        assert!(!stats.is_empty());
        assert_eq!(stats.len(), 147);
        Ok(())
    }

    #[tokio::test]
    async fn load_nodes_name_filter() -> Result<()> {
        let mut task = V2rayTask::with_default();
        task.fp = FilterProperty {
            node_name_regex: Some("安徽→香港01".to_owned()),
        };
        task.load_nodes().await?;
        let stats = task.node_stats.lock().await;
        assert_eq!(stats.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn ping_task_test() -> Result<()> {
        let mut task = V2rayTask::with_default();
        task.fp = FilterProperty {
            node_name_regex: Some("安徽→香港01".to_owned()),
        };
        task.load_nodes().await?;
        assert_eq!(task.node_stats.lock().await.len(), 1);

        update_tcp_ping_stats(task.node_stats.clone(), &task.vp, &task.pp).await?;

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
        task.fp = FilterProperty {
            node_name_regex: Some("安徽→香港01".to_owned()),
        };
        task.load_nodes().await?;

        {
            let mut nodes = task.node_stats.lock().await;
            assert_eq!(nodes.len(), 1);
            nodes[0].0.add = Some("non.host.address".to_owned());
        }

        let res = update_tcp_ping_stats(task.node_stats.clone(), &task.vp, &task.pp).await;
        assert!(res.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn update_subscription_test() -> Result<()> {
        let subscpt = SubscriptionProperty::default();
        let start =  Instant::now();
        update_subscription(&subscpt.url, &subscpt.path).await?;
        let modified_time = File::open(subscpt.path).await?.metadata().await?.modified()?;
        let elapsed = modified_time.elapsed()?;
        log::debug!("modified time: {:?}, elapsed: {:?}", modified_time, elapsed);
        assert!(elapsed < Instant::now() - start );
        Ok(())
    }

    #[tokio::test]
    async fn auto_update_subscription() -> Result<()> {
        let mut task = V2rayTask::with_default();
        task.subspt_property.update_interval = Duration::from_secs(3);
        // mock failure
        // task.subspt_property.url = "test.a.b".to_owned();
        task.auto_update_subscription().await?;
        sleep(Duration::from_secs(10)).await;
        Ok(())
    }

    // #[tokio::test]
    // async fn auto_ping_task_test() -> Result<()> {
    //     let mut task = V2rayTask::with_default();
    //     task.fp = Arc::new(FilterProperty {
    //         node_name_regex: Some("安徽→香港01".to_owned()),
    //     });
    //     task.ptp.ping_interval = Duration::from_secs(1);
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

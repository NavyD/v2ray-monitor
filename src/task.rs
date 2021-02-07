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
use reqwest::Proxy;
use tokio::{
    fs::{write, File},
    process::Child,
    sync::Mutex,
    time::{self as atime},
};

#[derive(Debug, Clone)]
pub struct FilterProperty {
    pub node_name_regex: Option<String>,
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

struct AutoTcpPingProperty {
    ping_interval: Duration,
    max_retries_failed: usize,
    min_retry_interval: Duration,
    max_retry_interval: Duration,
}

impl Default for AutoTcpPingProperty {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(60 * 10),
            max_retries_failed: 3,
            max_retry_interval: Duration::from_secs(60),
            min_retry_interval: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, Clone)]
struct SwitchProperty {
    check_url: String,
    check_timeout: Duration,

    lb_nodes_size: u8,

    max_retries_failed: usize,
    max_retry_interval: Duration,
    min_retry_interval: Duration,
}

impl Default for SwitchProperty {
    fn default() -> Self {
        Self {
            check_url: "https://www.google.com/gen_204".to_owned(),
            check_timeout: Duration::from_secs(3),
            lb_nodes_size: 3,
            max_retries_failed: 3,
            max_retry_interval: Duration::from_secs(30),
            min_retry_interval: Duration::from_secs(1),
        }
    }
}

static V2_CHILD: Lazy<Mutex<Option<Child>>> = Lazy::new(|| Mutex::new(None));

pub struct V2rayTask {
    node_stats: Arc<Mutex<Vec<(Node, TcpPingStatistic)>>>,
    tcp_ping_property: AutoTcpPingProperty,
    subscpt_property: SubscriptionProperty,
    switch_property: SwitchProperty,
    v2_property: V2rayProperty,
    ping_property: PingProperty,
    pub filter_property: FilterProperty,
}

impl V2rayTask {
    pub fn with_default() -> Self {
        Self {
            node_stats: Arc::new(Mutex::new(vec![])),
            tcp_ping_property: AutoTcpPingProperty::default(),
            subscpt_property: SubscriptionProperty::default(),
            switch_property: SwitchProperty::default(),
            v2_property: V2rayProperty::default(),
            ping_property: PingProperty::default(),
            filter_property: FilterProperty::default(),
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        self.load_nodes().await?;
        self.auto_ping();
        self.auto_update_subscription().await?;
        self.auto_swith().await;
        Ok(())
    }

    /// 根据订阅文件自动更新并加载到内存中。
    ///
    async fn auto_update_subscription(&mut self) -> Result<()> {
        let url = self.subscpt_property.url.clone();
        let path = self.subscpt_property.path.clone();
        let interval = self.subscpt_property.update_interval;
        let max_retries_failed = self.subscpt_property.max_retries_failed;
        let (min_itv, max_itv) = (
            self.subscpt_property.min_retry_interval,
            self.subscpt_property.max_retry_interval,
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

        let path = &self.subscpt_property.path;
        log::debug!("loading nodes from path: {}", path);
        let name_regex = if let Some(r) = &self.filter_property.node_name_regex {
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

    async fn auto_swith(&mut self) {
        let check_url = self.switch_property.check_url.clone();
        let timeout = self.switch_property.check_timeout;
        let max_retries_failed = self.switch_property.max_retries_failed;
        let (min_itv, max_itv) = (
            self.switch_property.min_retry_interval,
            self.switch_property.max_retry_interval,
        );
        let selected_size = self.switch_property.lb_nodes_size as usize;
        let node_stats = self.node_stats.clone();
        let vp = self.v2_property.clone();

        let proxy: Option<String> = self
            .v2_property
            .port
            .as_ref()
            .map(|p| "socks5://127.0.0.1:".to_string() + &p.to_string());

        tokio::spawn(async move {
            let check_url = check_url.to_owned();
            let task =
                Arc::new(move || check_networking_owned(check_url.clone(), timeout, proxy.clone()));

            let mut last_checked = false;
            let mut max_retries = max_retries_failed;

            loop {
                log::debug!("retrying check networking");
                match retry_on_owned(
                    task.clone(),
                    next_beb_interval(min_itv, max_itv),
                    max_retries,
                    last_checked,
                )
                .await
                {
                    Ok(retries) => {
                        if retries <= max_retries && !last_checked {
                            max_retries = std::usize::MAX;
                        }
                        // 上次是失败时 这次是做失败时重试 后成功退出
                        if !last_checked {
                            log::debug!("found networking problem on success retries: {}", retries);
                        }
                        // 这次做成功时重试 后失败退出
                        else {
                            max_retries = max_retries_failed;
                            log::debug!("recovery networking on failed retries: {}", retries);
                        }
                    }
                    // 重试次数达到max_retries
                    Err(e) => {
                        if !last_checked {
                            log::debug!("switching nodes");
                            if let Err(e) =
                                switch_v2ray(node_stats.clone(), selected_size, &vp).await
                            {
                                log::warn!("switch v2ray failed: {}", e);
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

    /// 成功ping时使用默认的interval，如果失败则使用`PingTaskProperty::max_retry_interval`不断重试
    fn auto_ping(&mut self) {
        let ping_interval = self.tcp_ping_property.ping_interval;
        let vp = Arc::new(self.v2_property.clone());
        let pp = Arc::new(self.ping_property.clone());
        let stats_lock = self.node_stats.clone();
        let max_retries = self.tcp_ping_property.max_retries_failed;
        let (min_itv, max_itv) = (
            self.tcp_ping_property.min_retry_interval,
            self.tcp_ping_property.max_retry_interval,
        );

        tokio::spawn(async move {
            loop_with_interval(
                move || {
                    let vp = vp.clone();
                    let pp = pp.clone();
                    let stats_lock = stats_lock.clone();
                    let task = move || {
                        update_tcp_ping_stats_owned(stats_lock.clone(), vp.clone(), pp.clone())
                    };
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

async fn switch_v2ray(
    node_stats: Arc<Mutex<Vec<(Node, TcpPingStatistic)>>>,
    selected_size: usize,
    vp: &V2rayProperty,
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
        nodes.iter().map(|v| (v.0.clone(), v.1.clone())).collect::<Vec<_>>()
    };

    if log::log_enabled!(log::Level::Info) {
        let nodes = nodes
            .iter()
            .map(|(n, ps)| (n.remark.as_deref(), ps))
            .collect::<Vec<_>>();
        log::info!("switching with selected nodes: {:?}", nodes);
    }

    let nodes = nodes.iter().map(|v| &v.0).collect::<Vec<_>>();
    let child = restart_load_balance(&vp, &nodes, V2_CHILD.lock().await.take()).await?;
    V2_CHILD.lock().await.replace(child);
    Ok(())
}

fn next_beb_interval(min_itv: Duration, max_itv: Duration) -> impl Fn(Duration, usize) -> Duration {
    move |last, _retries| {
        let next = last + last;
        if next < min_itv {
            min_itv
        } else if next > max_itv {
            max_itv
        } else {
            next
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
                log::warn!(
                    "use max retry interval {:?} for loop failed: {}",
                    max_retry_interval,
                    e
                );
                max_retry_interval
            }
        };
        log::debug!("loop sleeping with interval: {:?}", interval);
        sleep(interval).await;
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
        retry_count += 1;
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
        last_itv = next_itv_func(last_itv, retry_count);
        log::debug!(
            "sleeping {:?} on retries: {}, max retries: {}",
            last_itv,
            retry_count,
            max_retries
        );
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
        task.filter_property = FilterProperty {
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
        task.filter_property = FilterProperty {
            node_name_regex: Some("安徽→香港01".to_owned()),
        };
        task.load_nodes().await?;
        assert_eq!(task.node_stats.lock().await.len(), 1);

        update_tcp_ping_stats(
            task.node_stats.clone(),
            &task.v2_property,
            &task.ping_property,
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
        task.filter_property = FilterProperty {
            node_name_regex: Some("安徽→香港01".to_owned()),
        };
        task.load_nodes().await?;

        {
            let mut nodes = task.node_stats.lock().await;
            assert_eq!(nodes.len(), 1);
            nodes[0].0.add = Some("non.host.address".to_owned());
        }

        let res = update_tcp_ping_stats(
            task.node_stats.clone(),
            &task.v2_property,
            &task.ping_property,
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
        task.filter_property = FilterProperty {
            // node_name_regex: Some("安徽→香港01".to_owned()),
            node_name_regex: Some("香港01".to_owned()),
        };
        let sp = task.switch_property.clone();
        // task.vp;
        task.load_nodes().await?;

        update_tcp_ping_stats(
            task.node_stats.clone(),
            &task.v2_property,
            &task.ping_property,
        )
        .await?;

        switch_v2ray(task.node_stats, 3, &task.v2_property).await?;
        check_networking(
            &sp.check_url,
            sp.check_timeout,
            Some(&("socks5://127.0.0.1:".to_owned() + &task.v2_property.port.unwrap().to_string())),
        )
        .await?;
        Ok(())
    }

    
    // #[tokio::test]
    // async fn auto_update_subscription() -> Result<()> {
    //     let mut task = V2rayTask::with_default();
    //     task.subscpt_property.update_interval = Duration::from_secs(3);
    //     // mock failure
    //     // task.subspt_property.url = "test.a.b".to_owned();
    //     task.auto_update_subscription().await?;
    //     sleep(Duration::from_secs(10)).await;
    //     Ok(())
    // }

    // #[tokio::test]
    // async fn auto_swith_test() -> Result<()> {
    //     let mut task = V2rayTask::with_default();
    //     task.filter_property = FilterProperty {
    //         node_name_regex: Some("安徽→香港01".to_owned()),
    //         // node_name_regex: Some("香港01".to_owned()),
    //     };
    //     task.load_nodes().await?;

    //     update_tcp_ping_stats(
    //         task.node_stats.clone(),
    //         &task.v2_property,
    //         &task.ping_property,
    //     )
    //     .await?;

    //     task.auto_swith().await;
    //     sleep(Duration::from_secs(150)).await;
    //     Ok(())
    // }

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

use std::{
    fmt::{Debug, Display},
    future::Future,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

pub mod filter;
pub mod switch;
pub mod v2ray_task_config;
pub mod v2ray_tasks;
pub mod tcp_ping;

use anyhow::{anyhow, Result};

use chrono::{Date, DateTime, Local, NaiveTime, Utc};
use humantime::{format_rfc3339, Timestamp};
use log::log_enabled;
use parking_lot::Mutex;
use tokio::time::{sleep, timeout};

use self::v2ray_task_config::RetryProperty;

#[derive(Clone)]
pub struct RetryService<F> {
    prop: Arc<RetryProperty>,
    func: Arc<F>,
}

impl<F, Fut> RetryService<F>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<()>>,
{
    pub fn new(prop: RetryProperty, func: F) -> Self {
        Self {
            prop: Arc::new(prop),
            func: Arc::new(func),
        }
    }

    pub async fn retry_on(&self, succ_or_fail: bool) -> Result<(usize, Duration)> {
        let prop = &self.prop;
        let name = if !succ_or_fail { "success" } else { "failed" };
        let next_interval = prop.interval_algo.next();
        let max_retries = prop.count;
        let mut last_itv = Duration::from_nanos(0);

        let mut half_start: Option<SystemTime> = if let Some(half) = &prop.half {
            let time = half.start.parse::<NaiveTime>()?;
            let start = Local::today().and_time(time).unwrap();
            log::trace!("half start: {:?}", start);
            Some(start.into())
        } else {
            None
        };

        let mut retry_count = 0;
        let start = Instant::now();

        while self.is_retried(succ_or_fail).await {
            retry_count += 1;
            if retry_count == std::usize::MAX {
                log::warn!("overflow usize retry_count. reset 0");
                retry_count = 0;
            }
            if retry_count > max_retries {
                return Err(anyhow!(
                    "retrying on {} reaches max retries {} exit",
                    name,
                    max_retries
                ));
            }

            last_itv = if let Some(start) = half_start.as_mut() {
                get_half_duration(start, prop.half.as_ref().unwrap().interval)?
                    .unwrap_or_else(|| next_interval(last_itv, retry_count))
            } else {
                next_interval(last_itv, retry_count)
            };
            if log::log_enabled!(log::Level::Debug) {
                log::debug!(
                    "sleeping {:?} until {:?} on retries: {}, max retries: {}",
                    last_itv,
                    Local::now() + chrono::Duration::from_std(last_itv)?,
                    retry_count,
                    max_retries
                );
            }
            sleep(last_itv).await;
        }
        let d = Instant::now() - start;
        log::debug!(
            "{} exit after {} retries, it takes {:?}",
            name,
            retry_count,
            d
        );
        Ok((retry_count, d))
    }

    async fn is_retried(&self, succ_or_fail: bool) -> bool {
        let func = &*self.func;
        let res = if let Some(timeout_interval) = self.prop.once_timeout {
            timeout(timeout_interval, func())
                .await
                .map_err(Into::into)
                .and_then(|e| e)
        } else {
            func().await
        };

        if let Err(e) = &res {
            log::debug!("an error occurred while retrying execution: {}", e);
        }
        (succ_or_fail && res.is_ok()) || (!succ_or_fail && res.is_err())
    }
}

fn get_half_duration(start: &mut SystemTime, interval: Duration) -> Result<Option<Duration>> {
    // 1.当前时间date与start拼接生成下次要暂停的时间点
    let end = *start + interval;
    let now = SystemTime::now();
    if *start <= now && end > now {
        let d = end.duration_since(now)?;
        return Ok(Some(d));
    }
    if now >= end {
        let offset = Duration::from_secs(60 * 60 * 24);
        *start += offset;
        log::trace!(
            "change start to {:?}, offset: {:?}",
            Into::<DateTime<Local>>::into(*start),
            offset
        );
    }
    Ok(None)
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
                log::error!(
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

pub fn find_v2ray_bin_path() -> Result<String> {
    find_bin_path("v2ray")
}

pub fn find_bin_path(name: &str) -> Result<String> {
    std::env::var_os("PATH")
        .and_then(|val| {
            std::env::split_paths(&val).find_map(|path| {
                if path.is_file() && path.ends_with(name) {
                    return Some(path);
                }
                let path = path.join(name);
                if path.is_file() {
                    return Some(path);
                }
                None
            })
        })
        .and_then(|path| path.to_str().map(|s| s.to_owned()))
        .ok_or_else(|| anyhow::anyhow!("not found {} in env var PATH", name))
}

#[async_trait::async_trait]
pub trait TaskRunnable {
    async fn run(&self) -> Result<()>;
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum RetryIntevalAlgorithm {
    Beb {
        #[serde(with = "humantime_serde")]
        min: Duration,
        #[serde(with = "humantime_serde")]
        max: Duration,
    },
}

impl RetryIntevalAlgorithm {
    pub fn next(&self) -> impl Fn(Duration, usize) -> Duration {
        match *self {
            Self::Beb { min, max } => move |last: Duration, _retries: usize| {
                let next = last + last;
                if next < min {
                    min
                } else if next > max {
                    max
                } else {
                    next
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_v2ray_bin_path_test() -> Result<()> {
        let path = find_bin_path("v2ray")?;
        assert!(path.contains("v2ray"));
        Ok(())
    }

    #[test]
    fn not_found_bin_path() {
        assert!(find_bin_path("__no_exist_v2ray_").is_err());
    }

    #[tokio::test]
    async fn retry_timeout() -> Result<()> {
        async fn task() -> Result<()> {
            let d = Duration::from_secs(1);
            sleep(d).await;
            Err(anyhow!("test error for sleep {:?}", d))
        }
        let mut prop = get_retry_prop()?;
        prop.once_timeout = Some(Duration::from_millis(10000));
        let retry = RetryService::new(prop, task);
        assert!(retry.retry_on(false).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn retry_half() -> Result<()> {
        async fn task() -> Result<()> {
            let d = Duration::from_secs(1);
            sleep(d).await;
            Err(anyhow!("test error for sleep {:?}", d))
        }
        let mut prop = get_retry_prop()?;
        let mut half = prop.half.as_mut().unwrap();
        let min = Duration::from_millis(100);
        let max = Duration::from_millis(500);
        prop.count = 2;
        half.start = Local::now().format("%H:%M:%S").to_string();
        prop.interval_algo = RetryIntevalAlgorithm::Beb { min, max };

        let retry = RetryService::new(prop, task);
        let out = timeout(max * 2, retry.retry_on(false)).await;
        assert!(out.is_err());

        let mut prop = get_retry_prop()?;
        let mut half = prop.half.as_mut().unwrap();
        half.start = (Local::now() + chrono::Duration::from_std(max * 10)?).to_string();

        let retry = RetryService::new(prop, task);
        let out = timeout(max * 4, retry.retry_on(false)).await;
        assert!(out.is_ok());
        assert!(out.unwrap().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn half_during_execution() -> Result<()> {
        async fn task() -> Result<()> {
            let d = Duration::from_millis(500);
            sleep(d).await;
            Err(anyhow!("test error for sleep {:?}", d))
        }
        let mut prop = get_retry_prop()?;
        let mut half = prop.half.as_mut().unwrap();
        let min = Duration::from_millis(100);
        let max = Duration::from_millis(500);
        prop.count = 2;
        half.start = (Local::now() + chrono::Duration::from_std(max * 10)?)
            .format("%H:%M:%S")
            .to_string();
        prop.interval_algo = RetryIntevalAlgorithm::Beb { min, max };

        let retry = RetryService::new(prop, task);
        let out = timeout(max * 2, retry.retry_on(false)).await;
        assert!(out.is_err());

        Ok(())
    }

    #[tokio::test]
    async fn get_half_durationtest() -> Result<()> {
        let interval = Duration::from_secs(10);
        // now .. start 不休眠
        let mut start = SystemTime::now() + interval / 2;
        let half = get_half_duration(&mut start, interval)?;
        assert!(half.is_none());

        // start .. now .. end 休眠 end - now
        let mut start = SystemTime::now() - interval / 2;
        let half = get_half_duration(&mut start, interval)?;
        assert!(half.is_some());
        assert!(half < Some(interval));

        // start..end .. now 不休眠 改变start为下一天
        let mut start = SystemTime::now() - interval * 2;
        let half = get_half_duration(&mut start, interval)?;
        assert!(half.is_none());
        assert!(start > SystemTime::now() + Duration::from_secs(60 * 60 * 23));
        Ok(())
    }

    fn get_retry_prop() -> Result<RetryProperty> {
        let content = r#"
retry:
count: 3
interval_algo:
    type: Beb
    min: 100ms
    max: 2s
half:
    start: "02:00:00"
    interval: 6h"#;
        serde_yaml::from_str::<RetryProperty>(content).map_err(Into::into)
    }
}

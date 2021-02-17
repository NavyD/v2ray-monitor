use std::{
    cell::RefCell,
    fmt::{Debug, Display},
    future::Future,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

pub mod filter;
pub mod switch;
pub mod tcp_ping;
pub mod v2ray_task_config;
pub mod v2ray_tasks;
pub mod subscription;

use anyhow::{anyhow, Result};

use chrono::{Date, DateTime, Local, NaiveTime, Utc};

use log::trace;
use parking_lot::Mutex;
use tokio::time::{sleep, timeout};

use self::v2ray_task_config::{RetryIntevalAlgorithm, RetryProperty};
struct Stat {
    half_start: Option<SystemTime>,
    failed_count: usize,
    success_count: usize,
    last_checked: bool,
}

pub struct RetryService {
    prop: RetryProperty,
    stat: Arc<Mutex<Stat>>,
}

impl RetryService {
    pub fn new(prop: RetryProperty) -> Self {
        let half_start: Option<SystemTime> = if let Some(half) = &prop.half {
            let time = half
                .start
                .parse::<NaiveTime>()
                .unwrap_or_else(|e| panic!("parse error half.start: {}, {}", half.start, e));
            let start = Local::today().and_time(time).unwrap();
            log::trace!("half start: {:?}", start);
            Some(start.into())
        } else {
            None
        };
        Self {
            prop,
            stat: Arc::new(Mutex::new(Stat {
                half_start,
                failed_count: 0,
                success_count: 0,
                last_checked: false,
            })),
        }
    }

    fn retry_count(&self, ok_or_err: bool) -> usize {
        let mut stat = self.stat.lock();
        let retry = if ok_or_err {
            &mut stat.success_count
        } else {
            &mut stat.failed_count
        };
        *retry
    }

    fn retry_count_incr(&self, ok_or_err: bool) -> usize {
        let mut stat = self.stat.lock();
        let retry = if ok_or_err {
            &mut stat.success_count
        } else {
            &mut stat.failed_count
        };
        *retry += 1;
        *retry
    }

    /// 根据在func执行结果决定是否重新执行func
    ///
    /// 如果
    pub async fn retry_on<F, Fut>(&self, func: F, ok_or_err: bool) -> Result<(usize, Duration)>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let name = if !ok_or_err { "success" } else { "failed" };
        log::trace!("Retry on {}", name);

        let prop = &self.prop;
        let next_interval = prop.interval_algo.next(&self);
        let mut cur_interval = Duration::from_nanos(0);
        let func = Arc::new(func);

        let mut cur_retries = 0;
        // 成功重试时  使用最大值重试
        let max_retries = if ok_or_err {
            std::usize::MAX
        } else {
            prop.count
        };

        let start = Instant::now();
        while self.is_retried(func.clone(), ok_or_err).await {
            // 错误重试
            if !ok_or_err {
                self.stat.lock().failed_count += 1;
            }
            cur_retries += 1;
            if cur_retries == std::usize::MAX {
                log::warn!("overflow usize retry_count. reset 0");
                cur_retries = 0;
            }
            // 错误重试次数限制
            if cur_retries > max_retries {
                let d = Instant::now() - start;
                log::debug!(
                    "Retry on {} and exit after reaching the maximum: {}, it takes {:?}",
                    name,
                    max_retries,
                    d
                );
                return Err(anyhow!("retrying reaches max retries {} exit", max_retries));
            }

            cur_interval = next_interval(cur_interval);
            log::debug!(
                "sleeping {:?} on retries: {}, max retries: {}",
                cur_interval,
                cur_retries,
                max_retries
            );
            sleep(cur_interval).await;
        }
        let d = Instant::now() - start;
        let retry_count = self.retry_count(ok_or_err);
        log::debug!(
            "Retry on {} and exit after {} retries, it takes {:?}",
            name,
            retry_count,
            d
        );
        // 在错误重试时 成功退出没有问题了 重置failed count
        if !ok_or_err {
            self.stat.lock().failed_count = 0;
        }
        Ok((retry_count, d))
    }

    async fn is_retried<F, Fut>(&self, func: Arc<F>, ok_or_err: bool) -> bool
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<()>>,
    {
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
        (ok_or_err && res.is_ok()) || (!ok_or_err && res.is_err())
    }
}

fn get_or_mod_start_half_duration(
    start: &mut SystemTime,
    interval: Duration,
) -> Result<Option<Duration>> {
    // 1.当前时间date与start拼接生成下次要暂停的时间点
    let end = *start + interval;
    let now = SystemTime::now();
    // 现在可以休眠了
    if *start <= now && end > now {
        let next = end.duration_since(now).map_err(|e| {
            anyhow!(
                "time elapsed error: {}, end {:?}, now: {:?}",
                e,
                DateTime::<Local>::from(end),
                DateTime::<Local>::from(now)
            )
        })?;
        log::info!(
            "Start to sleep for {:?} until {}",
            next,
            DateTime::<Local>::from(now + next)
        );
        return Ok(Some(next));
    }
    // 现在过了休眠区间 设置start为下一天
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

impl RetryIntevalAlgorithm {
    pub fn next<'a>(
        &self,
        retry: &'a RetryService,
    ) -> Box<dyn Fn(Duration) -> Duration + 'a + Send + Sync> {
        use self::RetryIntevalAlgorithm::*;
        match *self {
            Beb { min, max } => Box::new(move |last: Duration| {
                log::debug!("{}", retry.prop.count);
                let next = last + last;
                if next < min {
                    min
                } else if next > max {
                    max
                } else {
                    next
                }
            }),
            SwitchBeb {
                min,
                max,
                switch_limit,
            } => Box::new(move |last: Duration| -> Duration {
                // 休眠检查
                if let Some(next) = retry.stat.lock().half_start.as_mut().and_then(|start| {
                    get_or_mod_start_half_duration(
                        start,
                        retry.prop.half.as_ref().unwrap().interval,
                    )
                    .unwrap()
                }) {
                    return next;
                }
                // switch切换
                if switch_limit * retry.prop.count <= retry.stat.lock().failed_count {
                    return max;
                }

                let next = last + last;
                if next < min {
                    min
                } else if next > max {
                    max
                } else {
                    next
                }
            }),
        }
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;

//     #[test]
//     fn find_v2ray_bin_path_test() -> Result<()> {
//         let path = find_bin_path("v2ray")?;
//         assert!(path.contains("v2ray"));
//         Ok(())
//     }

//     #[test]
//     fn not_found_bin_path() {
//         assert!(find_bin_path("__no_exist_v2ray_").is_err());
//     }

//     #[tokio::test]
//     async fn retry_timeout() -> Result<()> {
//         async fn task() -> Result<()> {
//             let d = Duration::from_secs(1);
//             sleep(d).await;
//             Err(anyhow!("test error for sleep {:?}", d))
//         }
//         let mut prop = get_retry_prop()?;
//         prop.once_timeout = Some(Duration::from_millis(10000));
//         let retry = RetryService::new(prop);
//         assert!(retry.retry_on(Arc::new(task), false).await.is_err());
//         Ok(())
//     }

//     #[tokio::test]
//     async fn retry_half() -> Result<()> {
//         async fn task() -> Result<()> {
//             let d = Duration::from_secs(1);
//             sleep(d).await;
//             Err(anyhow!("test error for sleep {:?}", d))
//         }
//         let mut prop = get_retry_prop()?;
//         let mut half = prop.half.as_mut().unwrap();
//         let min = Duration::from_millis(100);
//         let max = Duration::from_millis(500);
//         prop.count = 2;
//         half.start = Local::now().format("%H:%M:%S").to_string();
//         prop.interval_algo = RetryIntevalAlgorithm::Beb { min, max };

//         let retry = RetryService::new(prop);
//         let task = Arc::new(task);
//         let out = timeout(max * 2, retry.retry_on(task,false)).await;
//         assert!(out.is_err());

//         let mut prop = get_retry_prop()?;
//         let mut half = prop.half.as_mut().unwrap();
//         half.start = (Local::now() + chrono::Duration::from_std(max * 10)?).to_string();

//         let retry = RetryService::new(prop);
//         let out = timeout(max * 4, retry.retry_on(task, false)).await;
//         assert!(out.is_ok());
//         assert!(out.unwrap().is_err());
//         Ok(())
//     }

//     #[tokio::test]
//     async fn half_during_execution() -> Result<()> {
//         async fn task() -> Result<()> {
//             let d = Duration::from_millis(500);
//             sleep(d).await;
//             Err(anyhow!("test error for sleep {:?}", d))
//         }
//         let mut prop = get_retry_prop()?;
//         let mut half = prop.half.as_mut().unwrap();
//         let min = Duration::from_millis(100);
//         let max = Duration::from_millis(500);
//         prop.count = 2;
//         half.start = (Local::now() + chrono::Duration::from_std(max * 10)?)
//             .format("%H:%M:%S")
//             .to_string();
//         prop.interval_algo = RetryIntevalAlgorithm::Beb { min, max };

//         let retry = RetryService::new(prop);
//         let task = Arc::new(task);
//         let out = timeout(max * 2, retry.retry_on(task,false)).await;
//         assert!(out.is_err());

//         Ok(())
//     }

//     #[tokio::test]
//     async fn get_half_durationtest() -> Result<()> {
//         let interval = Duration::from_secs(10);
//         // now .. start 不休眠
//         let mut start = SystemTime::now() + interval / 2;
//         let half = get_half_duration(&mut start, interval)?;
//         assert!(half.is_none());

//         // start .. now .. end 休眠 end - now
//         let mut start = SystemTime::now() - interval / 2;
//         let half = get_half_duration(&mut start, interval)?;
//         assert!(half.is_some());
//         assert!(half < Some(interval));

//         // start..end .. now 不休眠 改变start为下一天
//         let mut start = SystemTime::now() - interval * 2;
//         let half = get_half_duration(&mut start, interval)?;
//         assert!(half.is_none());
//         assert!(start > SystemTime::now() + Duration::from_secs(60 * 60 * 23));
//         Ok(())
//     }

//     fn get_retry_prop() -> Result<RetryProperty> {
//         let content = r#"
// retry:
// count: 3
// interval_algo:
//     type: Beb
//     min: 100ms
//     max: 2s
// half:
//     start: "02:00:00"
//     interval: 6h"#;
//         serde_yaml::from_str::<RetryProperty>(content).map_err(Into::into)
//     }
// }

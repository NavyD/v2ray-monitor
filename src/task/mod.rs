use std::{
    fmt::{Debug, Display},
    sync::Arc,
    time::{Duration, Instant},
};

pub mod v2ray_task_config;
pub mod v2ray_tasks;

use anyhow::{anyhow, Result};
use futures::Future;

use tokio::time::sleep;

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

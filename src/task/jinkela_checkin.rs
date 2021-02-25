use std::time::Duration;

use super::{v2ray_task_config::JinkelaCheckinTaskProperty, RetryService};
use crate::client::jinkela::JinkelaClient;
use anyhow::Result;
use chrono::{DateTime, Local};
use tokio::time::sleep;

pub struct JinkelaCheckinTask {
    client: JinkelaClient,
    prop: JinkelaCheckinTaskProperty,
    retry_srv: RetryService,
}

impl JinkelaCheckinTask {
    pub fn new(prop: JinkelaCheckinTaskProperty) -> Self {
        Self {
            client: JinkelaClient::new(prop.client.clone()),
            retry_srv: RetryService::new(prop.retry.clone()),
            prop,
        }
    }

    pub async fn run(&self) -> Result<()> {
        let mut update_time = Local::today().and_time(self.prop.update_time).unwrap();
        loop {
            log::debug!(
                "starting jinkela checkin task. update time: {}",
                update_time
            );
            match self
                .retry_srv
                .retry_on(move || task(self.client.clone()), false)
                .await
            {
                Ok((retries, duration)) => {
                    log::info!(
                        "Check in task succeeded. retries: {}, duration: {:?}",
                        retries,
                        duration
                    );
                }
                Err(e) => {
                    log::error!("Check in task failed: {}", e);
                }
            }
            sleep_on(&mut update_time).await?;
        }
    }
}

static DAY: Duration = Duration::from_secs(60 * 60 * 24);

async fn sleep_on(update_time: &mut DateTime<Local>) -> Result<()> {
    let day = DAY;
    let next_time = *update_time + chrono::Duration::from_std(day)?;
    let now = Local::now();
    let duration = if now < *update_time {
        *update_time - now
    } else {
        next_time - now
    };
    log::info!(
        "jinkela checkin task sleeping {} until {} on update time {}",
        duration,
        now + duration,
        *update_time
    );
    sleep(duration.to_std()?).await;
    *update_time = next_time;
    log::trace!("next update_time: {}", *update_time);
    Ok(())
}

async fn task(client: JinkelaClient) -> Result<()> {
    client.login().await?;
    let body = client.checkin().await?;
    if body.ret == 0 {
        log::warn!("checkin failed ret: {}, msg: {}", body.ret, body.msg);
    } else {
        log::info!(
            "checkin successful msg: {}, traffic: {:?}",
            body.msg,
            body.traffic_info
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::time::timeout;

    use super::*;

    #[tokio::test]
    async fn basic() -> Result<()> {
        let prop = get_prop()?;
        let task = JinkelaCheckinTask::new(prop);
        let res = timeout(Duration::from_secs(2), task.run()).await;
        assert!(res.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn sleep_test() -> Result<()> {
        let offset = Duration::from_millis(200);
        let small_offset = Duration::from_millis(100);

        // sleep要休眠offset 少给超时
        let mut update_time = Local::now() - chrono::Duration::from_std(offset)?;
        assert!(timeout(offset - small_offset, sleep_on(&mut update_time))
            .await
            .is_err());

        // sleep要休眠offset 多给超时
        let mut update_time = Local::now() - chrono::Duration::from_std(offset)?;
        let next_time = update_time + chrono::Duration::from_std(DAY)?;
        assert!(timeout(offset + small_offset, sleep_on(&mut update_time))
            .await
            .is_ok());
        assert_eq!(update_time, next_time);

        // 上次更新的update_time
        let next_time = update_time + chrono::Duration::from_std(DAY)?;
        assert!(timeout(offset + small_offset, sleep_on(&mut update_time))
            .await
            .is_err());
        assert_eq!(update_time + chrono::Duration::from_std(DAY)?, next_time);
        // let res = timeout(offset + offset, sleep_on(&mut update_time)).await;

        Ok(())
    }

    fn get_prop() -> Result<JinkelaCheckinTaskProperty> {
        let content = r#"
    client:
        username: dhjnavyd@qq.com
        password: ycDUKD9g@FS595v
        base_url: https://jinkela.best
        timeout: 5s
    update_time: "01:00:00"
    retry: 
        count: 3
        interval_algo:
            type: Beb
            min: 5s
            max: 30min"#;
        serde_yaml::from_str::<JinkelaCheckinTaskProperty>(content).map_err(Into::into)
    }
}

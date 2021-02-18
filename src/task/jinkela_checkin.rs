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
            log::info!("The checkin task of jinkela is in progress. update time: {}", update_time);
            let client = self.client.clone();
            client.login().await?;

            match self
                .retry_srv
                .retry_on(move || task(client.clone()), false)
                .await
            {
                Ok((retries, duration)) => {
                    log::debug!(
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

async fn sleep_on(update_time: &mut DateTime<Local>) -> Result<()> {
    let day = Duration::from_secs(60 * 60 * 24);

    let now = Local::now();
    let duration = if now < *update_time {
        *update_time - now
    } else {
        now - *update_time
    };
    log::info!(
        "jinkela checkin task sleeping {} on update time {}",
        duration,
        *update_time
    );
    sleep(duration.to_std()?).await;

    *update_time = *update_time + chrono::Duration::from_std(day)?;
    log::debug!("update update_time: {}", *update_time);
    Ok(())
}

async fn task(client: JinkelaClient) -> Result<()> {
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
    use super::*;

    #[tokio::test]
    async fn basic() -> Result<()> {
        let prop = get_prop()?;
        let task = JinkelaCheckinTask::new(prop);
        task.run().await?;
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

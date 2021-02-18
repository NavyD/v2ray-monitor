use std::sync::Arc;

use anyhow::{anyhow, Result};

use parking_lot::Mutex;
use reqwest::{redirect::Policy, Client};
use serde::{Deserialize, Serialize};

use crate::task::v2ray_task_config::JinkelaClientProperty;

#[derive(Debug, Clone)]
pub struct JinkelaClient {
    prop: Arc<JinkelaClientProperty>,
    client: Client,
    logged: Arc<Mutex<bool>>,
}

impl JinkelaClient {
    pub fn new(prop: JinkelaClientProperty) -> Self {
        let prop = Arc::new(prop);
        let client = Client::builder()
            .timeout(prop.timeout)
            .cookie_store(true)
            .user_agent(DEFAULT_USER_AGENT)
            .redirect(Policy::none())
            .build()
            .unwrap();
        Self {
            logged: Arc::new(Mutex::new(false)),
            client,
            prop,
        }
    }

    pub async fn login(&self) -> Result<()> {
        let form = [
            ("email", &self.prop.username),
            ("passwd", &self.prop.password),
        ];
        let url = format!("{}/auth/login", self.prop.base_url);
        log::trace!("Signing in to jinkela url: {}, body form: {:?}", url, form);
        let resp = self.client.post(&url).form(&form).send().await?;
        let status = resp.status();
        if !status.is_success() {
            log::error!(
                "Login failed. status: {}, body: {}",
                status,
                resp.text().await?
            );
            return Err(anyhow!("Login failed. status: {}", status));
        }

        // save cookies
        // let mut cookie_str = self.cookie_str.lock();
        // cookie_str.clear();
        // let mut cookie_str = resp.cookies().fold(cookie_str, |mut s, c| {
        //     s.push_str(&format!("{}={}; ", c.name(), c.value()));
        //     s
        // });
        // let len = cookie_str.len();
        // cookie_str.remove(len - 2);

        let body = resp.json::<ResultInfo>().await?;
        if body.ret == 0 {
            log::error!("login failed ret: {}, msg: {}", body.ret, body.msg);
            return Err(anyhow!("login failed: {}", body.msg));
        }
        log::debug!("login successful info: {:?}", body);
        *self.logged.lock() = true;

        Ok(())
    }

    pub fn is_logged(&self) -> bool {
        *self.logged.lock()
    }

    pub async fn checkin(&self) -> Result<ResultInfo> {
        if !self.is_logged() {
            return Err(anyhow!("no login"));
        }
        let url = format!("{}/user/checkin", self.prop.base_url);
        log::trace!("check in to jinkela url: {}", url);
        let resp = self.client.post(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            log::error!(
                "checkin failed. status: {}, body: {}",
                status,
                resp.text().await?
            );
            return Err(anyhow!("Login failed. status: {}", status));
        }
        let body = resp.json::<ResultInfo>().await?;

        if body.ret == 0 {
            log::debug!("checkin failed ret: {}, msg: {}", body.ret, body.msg);
        } else {
            log::debug!(
                "checkin successful msg: {}, traffic: {:?}",
                body.msg,
                body.traffic_info
            );
        }

        Ok(body)
    }
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultInfo {
    pub msg: String,
    pub ret: u8,
    pub unflow_traffic: Option<usize>,
    pub traffic: Option<String>,
    pub traffic_info: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficInfo {
    pub un_used_traffic: String,
    pub last_used_traffic: String,
    pub today_used_traffic: String,
}

const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/87.0.4280.141 Safari/537.36";

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn basic() -> Result<()> {
        let prop = get_prop()?;
        let client = JinkelaClient::new(prop);
        client.login().await?;
        let info = client.checkin().await?;
        assert!(
            (info.ret == 0 && info.traffic_info.is_none())
                || (info.ret == 1 && info.traffic_info.is_some())
        );

        Ok(())
    }

    fn get_prop() -> Result<JinkelaClientProperty> {
        let content = r#"
username: dhjnavyd@qq.com
password: ycDUKD9g@FS595v
base_url: https://jinkela.best
timeout: 5s
checkin:
    update_time: "01:00:00"
    retry: 
        count: 3
        interval_algo:
            type: Beb
            min: 5s
            max: 30min
        once_timeout: 5s
      "#;
        serde_yaml::from_str(content).map_err(Into::into)
    }
}

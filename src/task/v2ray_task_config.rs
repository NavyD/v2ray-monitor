use std::{
    fmt::{Debug, Display},
    io::BufReader,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    node::{load_subscription_nodes_from_file, Node},
    v2ray::*,
};
use anyhow::{anyhow, Result};
use futures::Future;

use super::*;
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Proxy;
use serde::{Deserialize, Serialize};
use tokio::{
    fs::{write, File},
    process::Child,
    sync::Mutex,
    time::sleep,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RetryProperty {
    count: usize,
    min_interval: Duration,
    max_interval: Duration,
}

impl Default for RetryProperty {
    fn default() -> Self {
        Self {
            count: 3,
            min_interval: Duration::from_secs(2),
            max_interval: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubscriptionProperty {
    path: String,
    update_interval: Duration,
    url: String,
    retry_failed: RetryProperty,
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
            retry_failed: Default::default()
        }
    }
}
#[derive(Debug, Clone, Deserialize, Serialize)]
struct AutoTcpPingProperty {
    ping_interval: Duration,
    retry_failed: RetryProperty,
}

impl Default for AutoTcpPingProperty {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(60 * 10),
            retry_failed: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SwitchProperty {
    check_url: String,
    check_timeout: Duration,

    lb_nodes_size: u8,

    retry: RetryProperty,
}

impl Default for SwitchProperty {
    fn default() -> Self {
        Self {
            check_url: "https://www.google.com/gen_204".to_owned(),
            check_timeout: Duration::from_secs(3),
            lb_nodes_size: 3,
            retry: Default::default(),
        }
    }
}


#[derive(Debug, Deserialize, Serialize)]
pub struct V2rayTaskProperty {
    auto_ping: AutoTcpPingProperty,
    subscpt: SubscriptionProperty,
    switch: SwitchProperty,

    v2: V2rayProperty,
    ping: PingProperty,
    filter: FilterProperty,
}

#[cfg(test)]
mod tests {
    use std::fs::read_to_string;

    use super::*;

    #[test]
    fn test_name() -> anyhow::Result<()>{
        let content = read_to_string("/home/navyd/Workspaces/projects/router-tasks/config.yaml")?;
        let a: V2rayTaskProperty = serde_yaml::from_str(&content)?;
        log::debug!("{:?}", a);
        Ok(())
    }
}
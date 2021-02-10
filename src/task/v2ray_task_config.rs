use std::{fmt::Debug, time::Duration};

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Deserialize, Serialize, Copy)]
pub struct RetryProperty {
    pub count: usize,
    pub min_interval: Duration,
    pub max_interval: Duration,
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
    pub path: String,
    pub update_interval: Duration,
    pub url: String,
    pub retry_failed: RetryProperty,
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
            retry_failed: Default::default(),
        }
    }
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AutoTcpPingProperty {
    pub ping_interval: Duration,
    pub retry_failed: RetryProperty,
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
pub struct SwitchProperty {
    pub check_url: String,
    pub check_timeout: Duration,
    pub lb_nodes_size: u8,
    pub retry: RetryProperty,
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct V2rayProperty {
    pub bin_path: Option<String>,
    pub config_path: Option<String>,
    pub concurr_num: Option<usize>,
    pub port: Option<u16>,
}

impl Default for V2rayProperty {
    /// 设置bin path为PATH中的v2ra，，config置为Non，，concurr_num
    /// 设为cpu_nums
    ///
    /// # panic
    ///
    /// 如果未在PATH中找到v2ray
    fn default() -> Self {
        Self {
            bin_path: None,
            config_path: None,
            concurr_num: Some(num_cpus::get() * 2),
            port: Some(1080),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PingProperty {
    pub count: u8,
    pub ping_url: String,
    pub timeout: Duration,
}

impl Default for PingProperty {
    fn default() -> Self {
        PingProperty {
            count: 3,
            ping_url: "https://www.google.com/gen_204".into(),
            timeout: Duration::from_secs(3),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct V2rayTaskProperty {
    pub auto_ping: AutoTcpPingProperty,
    pub subscpt: SubscriptionProperty,
    pub switch: SwitchProperty,
    pub v2: V2rayProperty,
    pub ping: PingProperty,
    pub filter: FilterProperty,
}

#[cfg(test)]
mod tests {
    use std::fs::read_to_string;

    use super::*;

    #[test]
    fn test_name() -> anyhow::Result<()> {
        let content = read_to_string("config.yaml")?;
        let property: V2rayTaskProperty = serde_yaml::from_str(&content)?;
        log::debug!("property: {:?}", property);
        Ok(())
    }
}

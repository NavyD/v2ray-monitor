use std::{fmt::Debug, time::Duration};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FilterProperty {
    pub name_regex: Option<String>,
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
pub struct TcpPingProperty {
    pub ping_interval: Duration,
    pub filter: FilterProperty,
    pub retry_failed: RetryProperty,
    pub ping: PingProperty,
}

impl Default for TcpPingProperty {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(60 * 10),
            retry_failed: Default::default(),
            filter: FilterProperty { name_regex: None },
            ping: PingProperty::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SwitchFilterProperty {
    pub lb_nodes_size: u8,
    pub name_regex: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SwitchProperty {
    pub check_url: String,
    pub check_timeout: Duration,
    pub retry: RetryProperty,
    pub filter: SwitchFilterProperty,
    pub ssh: V2raySshProperty,
}

impl Default for SwitchProperty {
    fn default() -> Self {
        Self {
            check_url: "https://www.google.com/gen_204".to_owned(),
            check_timeout: Duration::from_secs(3),
            retry: Default::default(),
            ssh: Default::default(),
            filter: SwitchFilterProperty {
                lb_nodes_size: 3,
                name_regex: None,
            },
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct V2raySshProperty {
    pub bin_path: Option<String>,
    pub username: String,
    pub host: String,
    pub config_path: String,
}

impl Default for V2raySshProperty {
    /// 设置bin path为PATH中的v2ra，，config置为Non，，concurr_num
    /// 设为cpu_nums
    ///
    /// # panic
    ///
    /// 如果未在PATH中找到v2ray
    fn default() -> Self {
        Self {
            bin_path: None,
            username: "root".to_string(),
            host: "openwrt".to_string(),
            config_path: "/var/etc/ssrplus/tcp-only-ssr-retcp.json".to_string(),
        }
    }
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
    pub tcp_ping: TcpPingProperty,
    pub subscpt: SubscriptionProperty,
    pub switch: SwitchProperty,
    pub v2: V2rayProperty,
}

#[cfg(test)]
mod tests {
    use std::fs::read_to_string;

    use super::*;

    #[test]
    fn read_config_yaml() -> anyhow::Result<()> {
        let content = read_to_string("config.yaml")?;
        let property: V2rayTaskProperty = serde_yaml::from_str(&content)?;
        log::debug!("property: {:?}", property);
        Ok(())
    }
}

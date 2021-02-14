use std::{fmt::Debug, time::Duration};

use serde::{Deserialize, Serialize};

use super::find_bin_path;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubscriptionTaskProperty {
    pub path: String,
    pub update_interval: Duration,
    pub url: String,
    pub retry_failed: RetryProperty,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TcpPingTaskProperty {
    pub ping_interval: Duration,
    pub filter: TcpPingFilterProperty,
    pub retry_failed: RetryProperty,
    pub ping: PingProperty,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SwitchFilterProperty {
    pub lb_nodes_size: u8,
    pub name_regex: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SwitchTaskProperty {
    pub check_url: String,
    pub check_timeout: Duration,
    pub retry: RetryProperty,
    pub filter: SwitchFilterProperty,
    pub ssh: SshV2rayProperty,
    pub local: LocalV2rayProperty,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct V2rayTaskProperty {
    pub tcp_ping: TcpPingTaskProperty,
    pub subscpt: SubscriptionTaskProperty,
    pub switch: SwitchTaskProperty,
}

// ...

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
pub struct TcpPingFilterProperty {
    #[serde(default)]
    pub name_regex: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LocalV2rayProperty {
    /// 默认为在本地PATH变量找v2ray
    #[serde(default = "default_local_v2ray_bin_path")]
    pub bin_path: String,
    #[serde(default)]
    pub config_path: Option<String>,
}

fn default_local_v2ray_bin_path() -> String {
    find_bin_path("v2ray").expect("not found v2ray in path")
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SshV2rayProperty {
    pub bin_path: String,
    pub username: String,
    pub host: String,
    pub config_path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PingProperty {
    pub count: u8,
    pub ping_url: String,
    pub timeout: Duration,

    /// 表示v2ray并发tcp ping节点时的数量。默认为cpu数量
    #[serde(default = "num_cpus::get")]
    pub concurr_num: usize,
}

impl Default for PingProperty {
    fn default() -> Self {
        PingProperty {
            count: 3,
            ping_url: "https://www.google.com/gen_204".into(),
            timeout: Duration::from_secs(5),
            concurr_num: num_cpus::get(),
        }
    }
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

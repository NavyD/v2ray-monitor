use std::{fmt::Debug, time::Duration};

use chrono::NaiveTime;
use serde::{Deserialize, Serialize};

use super::find_v2ray_bin_path;
#[serde(deny_unknown_fields)]
#[derive(Debug, Deserialize, Serialize)]
pub struct V2rayTaskProperty {
    #[serde(default)]
    pub tcp_ping: TcpPingTaskProperty,
    pub subx: SubscriptionTaskProperty,
    pub switch: SwitchTaskProperty,
    pub v2ray: V2rayProperty,
    pub jinkela: Option<JinkelaCheckinTaskProperty>,
    #[serde(default)]
    pub dns: DnsFlushProperty,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubscriptionTaskProperty {
    /// 订阅文件路径。文件可以不存在，如果存在则会被更新覆盖
    pub path: String,
    /// 更新间隔。根据文件修改时间计算更新
    #[serde(with = "humantime_serde", default = "default_subx_update_interval")]
    pub update_interval: Duration,
    /// 订阅的url
    pub url: String,
    #[serde(default = "default_subx_retry")]
    pub retry: RetryProperty,
}

fn default_subx_update_interval() -> Duration {
    Duration::from_secs(60 * 60 * 24)
}
fn default_subx_retry() -> RetryProperty {
    RetryProperty {
        count: 5,
        interval_algo: RetryIntevalAlgorithm::Beb {
            min: Duration::from_secs(10),
            max: Duration::from_secs(30),
        },
        half: None,
        once_timeout: None,
    }
}

#[serde(deny_unknown_fields)]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TcpPingTaskProperty {
    /// 更新间隔
    #[serde(with = "humantime_serde", default = "default_tcpping_update_interval")]
    pub update_interval: Duration,
    /// 过滤属性
    #[serde(default)]
    pub filter: TcpPingFilterProperty,
    /// 错误重试
    #[serde(default = "default_tcpping_retry")]
    pub retry: RetryProperty,
    /// ping属性
    #[serde(default)]
    pub ping: PingProperty,
    /// 使用的v2ray类型
    #[serde(default)]
    pub v2_type: V2rayType,
}

fn default_tcpping_update_interval() -> Duration {
    Duration::from_secs(60 * 30)
}
fn default_tcpping_retry() -> RetryProperty {
    RetryProperty {
        count: 3,
        interval_algo: RetryIntevalAlgorithm::Beb {
            min: Duration::from_secs(3),
            max: Duration::from_secs(30),
        },
        half: None,
        once_timeout: None,
    }
}

impl Default for TcpPingTaskProperty {
    fn default() -> Self {
        Self {
            update_interval: default_tcpping_update_interval(),
            filter: Default::default(),
            retry: default_tcpping_retry(),
            ping: Default::default(),
            v2_type: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SwitchFilterProperty {
    pub lb_nodes_size: u8,
    pub name_regex: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SwitchTaskProperty {
    /// 检查切换是否有效的验证url
    #[serde(default = "default_switch_check_url")]
    pub check_url: String,
    /// 检查访问的超时时间
    #[serde(with = "humantime_serde", default = "default_switch_check_timeout")]
    pub check_timeout: Duration,
    /// 检查错误时重试
    #[serde(default = "default_switch_check_retry")]
    pub check_retry: RetryProperty,
    /// 是否使用代理检查网络可用.默认不使用。在使用路由器代理时有用
    #[serde(default)]
    pub check_with_proxy: bool,
    pub filter: SwitchFilterProperty,
    #[serde(default)]
    pub v2_type: V2rayType,

    /// 限制连续切换节点的间隔
    #[serde(with = "humantime_serde", default = "default_switch_limit_interval")]
    pub limit_interval: Duration,
    /// 监控网卡
    #[serde(default)]
    pub monitor: NetworkMonitorProperty,
}

fn default_switch_check_url() -> String {
    "https://www.google.com/gen_204".to_string()
}
fn default_switch_check_timeout() -> Duration {
    Duration::from_secs(2)
}
fn default_switch_check_retry() -> RetryProperty {
    RetryProperty {
        count: 7,
        interval_algo: RetryIntevalAlgorithm::Beb {
            min: Duration::from_millis(100),
            max: Duration::from_secs(2),
        },
        half: None,
        once_timeout: None,
    }
}
fn default_switch_limit_interval() -> Duration {
    Duration::from_secs(5)
}

// ...
#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum V2rayType {
    Local,
    Ssh,
}

impl Default for V2rayType {
    fn default() -> Self {
        Self::Local
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RetryProperty {
    pub count: usize,
    pub interval_algo: RetryIntevalAlgorithm,
    #[serde(with = "humantime_serde", default)]
    pub once_timeout: Option<Duration>,
    #[serde(default)]
    pub half: Option<RetryHalfProperty>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RetryHalfProperty {
    pub start: String,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct TcpPingFilterProperty {
    #[serde(default)]
    pub name_regex: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LocalV2rayProperty {
    /// 默认为在本地PATH变量找v2ray
    #[serde(default = "default_local_v2ray_bin_path")]
    pub bin_path: String,
    /// v2ray的配置文件，用于切换switch时应用
    #[serde(default)]
    pub config_path: Option<String>,
}

fn default_local_v2ray_bin_path() -> String {
    find_v2ray_bin_path().expect("not found v2ray in path")
}

impl Default for LocalV2rayProperty {
    fn default() -> Self {
        LocalV2rayProperty {
            bin_path: find_v2ray_bin_path().expect("not found v2ray in path"),
            config_path: None,
        }
    }
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
    /// 对一个节点ping的次数
    #[serde(default = "default_ping_count")]
    pub count: u8,
    /// 进行tcp ping时访问目标的url
    #[serde(default = "default_ping_url")]
    pub url: String,
    /// ping访问超时
    #[serde(with = "humantime_serde", default = "default_ping_timeout")]
    pub timeout: Duration,
    /// 表示v2ray并发tcp ping节点时的数量。默认为cpu数量
    #[serde(default = "num_cpus::get")]
    pub concurr_num: usize,
}

fn default_ping_count() -> u8 {
    3
}
fn default_ping_url() -> String {
    default_switch_check_url()
}
fn default_ping_timeout() -> Duration {
    Duration::from_secs(2)
}

impl Default for PingProperty {
    fn default() -> Self {
        Self {
            count: default_ping_count(),
            url: default_ping_url(),
            timeout: default_ping_timeout(),
            concurr_num: num_cpus::get(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct V2rayProperty {
    #[serde(default)]
    pub ssh: Option<SshV2rayProperty>,
    #[serde(default)]
    pub local: LocalV2rayProperty,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum RetryIntevalAlgorithm {
    Beb {
        #[serde(with = "humantime_serde")]
        min: Duration,
        #[serde(with = "humantime_serde")]
        max: Duration,
    },
    SwitchBeb {
        #[serde(with = "humantime_serde")]
        min: Duration,
        #[serde(with = "humantime_serde")]
        max: Duration,
        #[serde(default = "default_switch_limit")]
        switch_limit: usize,
    },
}

fn default_switch_limit() -> usize {
    3
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JinkelaCheckinTaskProperty {
    pub client: JinkelaClientProperty,

    #[serde(with = "naivetime_format")]
    pub update_time: NaiveTime,
    pub retry: RetryProperty,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JinkelaClientProperty {
    pub username: String,
    pub password: String,
    pub base_url: String,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DnsFlushProperty {
    /// 主机名列表
    #[serde(default = "default_dns_hosts")]
    pub hosts: Vec<String>,
    /// dns更新间隔 默认10min
    #[serde(with = "humantime_serde", default = "default_dns_update_interval")]
    pub update_interval: Duration,
    #[serde(default = "default_dns_retry")]
    pub retry: RetryProperty,
}

fn default_dns_hosts() -> Vec<String> {
    vec!["www.google.com".to_string()]
}
fn default_dns_update_interval() -> Duration {
    Duration::from_secs(60 * 10)
}
fn default_dns_retry() -> RetryProperty {
    RetryProperty {
        count: 5,
        interval_algo: RetryIntevalAlgorithm::Beb {
            min: Duration::from_secs(5),
            max: Duration::from_secs(30),
        },
        half: None,
        once_timeout: None,
    }
}

impl Default for DnsFlushProperty {
    fn default() -> Self {
        Self {
            hosts: default_dns_hosts(),
            retry: default_dns_retry(),
            update_interval: default_dns_update_interval(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkMonitorProperty {
    #[serde(with = "humantime_serde", default = "default_monitor_timeout")]
    /// 如果上次访问没有成功，到下次访问未成功时的间隔超过则表示需要切换
    pub timeout: Duration,
    #[serde(default = "default_monitor_ifname")]
    /// 要监控的网卡名。通常为`eth0`
    pub ifname: String,
    #[serde(default = "default_monitor_count")]
    /// 达到该次数后才切换，使用该参数可以缓冲一些不准确的切换，可能有网络抖动
    pub count: usize,
}

fn default_monitor_ifname() -> String {
    "eth0".to_string()
}
fn default_monitor_count() -> usize {
    3
}
fn default_monitor_timeout() -> Duration {
    Duration::from_secs(2)
}

impl Default for NetworkMonitorProperty {
    fn default() -> Self {
        Self {
            timeout: default_monitor_timeout(),
            count: default_monitor_count(),
            ifname: default_monitor_ifname(),
        }
    }
}

mod naivetime_format {
    use chrono::NaiveTime;
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(date: &NaiveTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&date.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<NaiveTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<NaiveTime>().map_err(serde::de::Error::custom)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use anyhow::Result;

        #[test]
        fn basic() -> Result<()> {
            let original = "01:00:00";
            let naive_time = original.parse::<NaiveTime>()?;

            #[derive(Deserialize)]
            struct A {
                #[serde(with = "self")]
                update_time: NaiveTime,
            }
            let json_str = r#"
            {
                "update_time": "01:00:00"
            }
          "#;
            let de_res = serde_json::from_str::<A>(json_str)?;
            assert_eq!(de_res.update_time, naive_time);
            assert_eq!(original, de_res.update_time.to_string());

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::read_to_string;

    use super::*;

    #[test]
    fn read_config_yaml() -> anyhow::Result<()> {
        let content = read_to_string("tests/data/config.yaml")?;
        let property = serde_yaml::from_str::<V2rayTaskProperty>(&content)?;
        log::debug!("property: {:?}", property);
        Ok(())
    }
}

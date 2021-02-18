use std::{fmt::Debug, time::Duration};

use chrono::NaiveTime;
use serde::{Deserialize, Serialize};

use super::find_v2ray_bin_path;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubscriptionTaskProperty {
    pub path: String,
    #[serde(with = "humantime_serde")]
    pub update_interval: Duration,
    pub url: String,
    pub retry_failed: RetryProperty,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TcpPingTaskProperty {
    #[serde(with = "humantime_serde")]
    pub ping_interval: Duration,
    #[serde(default)]
    pub filter: TcpPingFilterProperty,
    pub retry_failed: RetryProperty,
    pub ping: PingProperty,
    #[serde(default)]
    pub v2_type: V2rayType,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SwitchFilterProperty {
    pub lb_nodes_size: u8,
    pub name_regex: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SwitchTaskProperty {
    pub check_url: String,
    #[serde(with = "humantime_serde")]
    pub check_timeout: Duration,
    pub retry: RetryProperty,
    pub filter: SwitchFilterProperty,
    #[serde(default)]
    pub v2_type: V2rayType,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct V2rayTaskProperty {
    pub tcp_ping: TcpPingTaskProperty,
    pub subscpt: SubscriptionTaskProperty,
    pub switch: SwitchTaskProperty,
    pub v2ray: V2rayProperty,
    pub jinkela: Option<JinkelaCheckinTaskProperty>,
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
    #[serde(default)]
    pub config_path: Option<String>,
}

impl Default for LocalV2rayProperty {
    fn default() -> Self {
        LocalV2rayProperty {
            bin_path: find_v2ray_bin_path().expect("not found v2ray in path"),
            config_path: None,
        }
    }
}

fn default_local_v2ray_bin_path() -> String {
    find_v2ray_bin_path().expect("not found v2ray in path")
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
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,

    /// 表示v2ray并发tcp ping节点时的数量。默认为cpu数量
    #[serde(default = "num_cpus::get")]
    pub concurr_num: usize,
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

    use humantime::Timestamp;

    use super::*;

    #[test]
    fn read_config_yaml() -> anyhow::Result<()> {
        let content = read_to_string("tests/data/config.yaml")?;
        let property: V2rayTaskProperty = serde_yaml::from_str(&content)?;
        log::debug!("property: {:?}", property);
        let a = "2018-02-14T00:28:07".parse::<Timestamp>()?;
        // let a = humantime::format_rfc3339(SystemTime::now());
        log::info!("du: {:?}", a);
        Ok(())
    }
}

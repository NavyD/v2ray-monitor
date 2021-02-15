use anyhow::{anyhow, Error, Result};
use reqwest::Proxy;
use serde_json::{json, Value};
use thiserror::Error;

use super::node::Node;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("unsupported config expected: {0}, found: {1}")]
    Unsupported(String, String),
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// 应用nodes生成v2ray配置
///
/// 如果指定则使用local_port，否则依赖contents中的配置
///
/// # Errors
///
/// * 如果nodes为空
/// * node.host不一致时
/// * node.net不是`ws`类型时
/// * node关键字段中存在None
pub fn apply_config_value(
    value: &mut Value,
    nodes: &[&Node],
    local_port: Option<u16>,
) -> Result<String> {
    check_load_balance_nodes(nodes)?;

    let vnext = nodes
        .iter()
        .map(|node| {
            json!({
                "address": node.add.as_ref().expect("not found address"),
                "port": node.port.as_ref().expect("not found port"),
                "users": [
                    {
                        "id": node.id.as_ref().expect("not found id"),
                        "alterId": node.aid.as_ref().expect("not found alter_id")
                    }
                ]
            })
        })
        .collect::<Vec<_>>();
    apply(value, "outbound.settings.vnext", json!(vnext))?;

    let node = nodes[0];
    apply(
        value,
        "outbound.streamSettings.wsSettings.path",
        json!(node.path.as_ref().expect("not found path")),
    )?;

    apply(
        value,
        "outbound.streamSettings.wsSettings.headers.Host",
        json!(node.host.as_ref().expect("not found host")),
    )?;

    if let Some(port) = local_port {
        apply(value, "inbound.port", json!(port))?;
    } else {
        let port = get_mut(value, "inbound.port")?;
        log::debug!("use old inbound.port: {}", port);
    }

    Ok(value.to_string())
}

pub fn apply_config(contents: &str, nodes: &[&Node], local_port: Option<u16>) -> Result<String> {
    apply_config_value(&mut to_value(contents)?, nodes, local_port)
}

fn get_mut<'a>(contents: &'a mut Value, path: &str) -> Result<&'a mut Value> {
    path.split('.')
        .fold(Some(contents), |val, key| val.and_then(|v| v.get_mut(key)))
        .ok_or_else(|| anyhow!("read config error: {}", path))
}

fn get<'a>(contents: &'a Value, path: &str) -> Result<&'a Value> {
    path.split('.')
        .fold(Some(contents), |val, key| val.and_then(|v| v.get(key)))
        .ok_or_else(|| anyhow!("read config error: {}", path))
}

pub fn to_value(contents: &str) -> Result<Value> {
    let v = serde_json::from_str(contents)?;
    Ok(v)
}

pub fn get_port(contents: &str) -> Result<u16> {
    let mut value = to_value(contents)?;
    let port = get_mut(&mut value, "inbound.port")?;
    port.as_u64()
        .map(|v| v as u16)
        .ok_or_else(|| anyhow!("not found port"))
}

/// 从配置中读取出protocol与port组合host为一个标准proxy url。
///
/// 如：`socks5://192.168.1.1:9000`
///
/// # Errors
///
/// * config解析错误： protocol, port
/// * 如果当前protocol不支持
pub fn get_proxy_url(config: &str, host: &str) -> Result<String> {
    get_proxy_url_from_value(&to_value(config)?, host)
}

fn get_proxy_url_from_value(value: &Value, host: &str) -> Result<String> {
    let mut prot = get(value, "inbound.protocol")?
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("not found protocol"))?;
    let prot = if prot == "socks" {
        prot.push('5');
        prot
    } else if prot.contains("http") {
        prot
    } else {
        log::error!("found unsupported protocol {}", prot);
        return Err(ConfigError::Unsupported("inbound.protocol".to_string(), prot).into());
    };
    let port = get(value, "inbound.port")?
        .as_u64()
        .map(|v| v as u16)
        .ok_or_else(|| anyhow!("not found port"))?;
    Ok(format!("{}://{}:{}", prot, host, port))
}

// fn get_mut1<'a>(contents: &'a mut Value, path: &str) -> Result<&'a mut Value> {
//     path.split('.')
//         .fold(Some(contents), |val, key| {
//             val.and_then(|v| {
//                 // v.as_object_mut().map(|o| {
//                 //     o.iter_mut()
//                 //         .find(|(name, _)| name.to_ascii_lowercase() == key)
//                 //         .map(|(_, v)| v)
//                 // });
//                 if let Some(v) = v.get_mut(key) {
//                     return Some(v);
//                 }
//                 let a = v.clone();
//                 todo!()
//             })
//         })
//         .ok_or_else(|| anyhow!("read config error: {}", path))
// }

/// 使用path .分离出key找出对应的value并重置为new_val
fn apply(contents: &mut Value, path: &str, new_val: Value) -> Result<()> {
    let old = get_mut(contents, path)?;
    log::trace!("old {}: {:?}", path, old);
    *old = new_val;
    log::trace!("applied {}: {:?}", path, old);
    Ok(())
}

/// 检查nodes是否符合配置要求
fn check_load_balance_nodes(nodes: &[&Node]) -> std::result::Result<(), ConfigError> {
    if nodes.is_empty() {
        return Err(ConfigError::Invalid("empty nodes".to_string()));
    }
    // check nodes
    let first_host = nodes.first().unwrap().host.as_deref();
    for node in nodes {
        // check node net type
        if node.net.as_deref() != Some("ws") {
            return Err(ConfigError::Unsupported(
                "ws".to_string(),
                format!("{:?}", node.net),
            ));
        }
        // check multiple nodes host
        if node.host.as_deref() != first_host {
            return Err(ConfigError::Invalid(format!(
                "first host: {:?} inconsistent name -> host: {:?} -> {:?}",
                first_host, node.remark, node.host
            )));
        }
    }
    Ok(())
}

/// 应用nodes生成v2ray负载均衡配置
pub fn gen_load_balance_config(nodes: &[&Node], local_port: u16) -> Result<String> {
    let contents = r#"{
        "log": {
            "loglevel": "debug"
        },
        "inbound": {
            "settings": {
                "ip": "127.0.0.1"
            },
            "protocol": "socks",
            "port": 1080,
            "sniffing": {
                "enabled": true,
                "destOverride": [
                    "http",
                    "tls"
                ]
            },
            "listen": "127.0.0.1"
        },
        "outbound": {
            "settings": {
                "vnext": [
                    {
                        "address": "gz01.mobile.lay168.net",
                        "port": 61022,
                        "users": [
                            {
                                "alterId": 2,
                                "id": "55fb0457-d874-32c3-89a2-679fed6eabf1"
                            }
                        ]
                    }
                ]
            },
            "protocol": "vmess",
            "streamSettings": {
                "wsSettings": {
                    "headers": {
                        "Host": "hk02.az.jinkela.icu"
                    },
                    "path": "/hls"
                },
                "network": "ws"
            }
        }
    }"#;
    apply_config(contents, nodes, Some(local_port))
}

pub fn gen_tcp_ping_config(node: &Node, local_port: u16) -> Result<String> {
    gen_load_balance_config(&[node], local_port)
}

pub fn get_tcp_ping_config() -> String {
    let contents = r#"{
        "log": {
            "loglevel": "debug"
        },
        "inbound": {
            "settings": {
                "ip": "127.0.0.1"
            },
            "protocol": "socks",
            "port": 1080,
            "sniffing": {
                "enabled": true,
                "destOverride": [
                    "http",
                    "tls"
                ]
            },
            "listen": "127.0.0.1"
        },
        "outbound": {
            "settings": {
                "vnext": [
                    {
                        "address": "gz01.mobile.lay168.net",
                        "port": 61022,
                        "users": [
                            {
                                "alterId": 2,
                                "id": "55fb0457-d874-32c3-89a2-679fed6eabf1"
                            }
                        ]
                    }
                ]
            },
            "protocol": "vmess",
            "streamSettings": {
                "wsSettings": {
                    "headers": {
                        "Host": "hk02.az.jinkela.icu"
                    },
                    "path": "/hls"
                },
                "network": "ws"
            }
        }
    }"#;
    contents.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_test() -> Result<()> {
        let mut contents = json!({
            "a": {
                "b": 1
            }
        });
        let new_val = 10000000;
        apply(&mut contents, "a.b", json!(new_val))?;
        assert_eq!(contents["a"]["b"].as_u64(), Some(new_val));
        Ok(())
    }

    #[test]
    fn gen_config_nodes() -> Result<()> {
        let local_port = 1000;
        let config = gen_load_balance_config(&[&get_node(), &get_node()], local_port)?;
        log::debug!("config: {}", config);
        let node = get_node();
        assert!(!config.is_empty());

        let val: Value = serde_json::from_str(&config)?;
        let vnext_items = val["outbound"]["settings"]["vnext"].as_array().unwrap();
        assert_eq!(vnext_items.len(), 2);

        for item in vnext_items {
            check_vnext_item(item, &node);
        }
        Ok(())
    }

    #[test]
    fn apply_config_one_node() -> Result<()> {
        let contents = get_config_string();
        let node = get_node();
        let new_port = 10800;
        let new_config: Value =
            serde_json::from_str(&apply_config(&contents, &[&node], Some(new_port))?)?;

        assert_eq!(
            new_config["inbound"]["port"].as_u64(),
            Some(new_port as u64)
        );

        let vnext = new_config["outbound"]["settings"]["vnext"]
            .as_array()
            .unwrap();
        assert_eq!(vnext.len(), 1);
        check_vnext_item(&vnext[0], &node);

        check_ws_sets(
            &new_config["outbound"]["streamSettings"]["wsSettings"],
            &node,
        );
        Ok(())
    }

    #[test]
    fn apply_config_more_nodes() -> Result<()> {
        let contents = get_config_string();

        let mut node = get_node();
        node.add = Some("node.addr".into());
        let nodes = [&node, &get_node()];

        let new_port = 10800;
        let new_config: Value = serde_json::from_str(&apply_config(
            &contents,
            // 不一样的node
            &nodes,
            Some(new_port),
        )?)?;

        assert_eq!(
            new_config["inbound"]["port"].as_u64(),
            Some(new_port as u64)
        );

        let vnext = new_config["outbound"]["settings"]["vnext"]
            .as_array()
            .unwrap();
        assert_eq!(vnext.len(), 2);
        for i in 0..vnext.len() {
            check_vnext_item(&vnext[i], nodes[i]);
        }
        Ok(())
    }

    #[test]
    fn proxy_url_parse() -> Result<()> {
        let host = "127.0.0.1";
        let mut config = to_value(&get_config_string())?;
        let mut test = |prot: &str| -> Result<()> {
            *get_mut(&mut config, "inbound.protocol")? = json!(prot);
            let prot = if prot == "socks" { "socks5" } else { prot };
            if get_proxy_url_from_value(&config, host)? != format!("{}://{}:1080", prot, host) {
                Err(anyhow!("not inconsistent"))
            } else {
                Ok(())
            }
        };
        test("socks")?;
        test("http")?;
        test("https")?;

        assert!(test("nothings__").is_err());
        Ok(())
    }

    fn check_vnext_item(item: &Value, node: &Node) {
        assert_eq!(item["address"].as_str(), node.add.as_deref());
        assert_eq!(item["port"].as_u64(), node.port.map(|p| p as u64));

        let users = &item["users"];
        assert_eq!(users.as_array().map(|a| a.len()), Some(1));
        assert_eq!(users[0]["alterId"].as_u64(), node.aid.map(|a| a as u64));
        assert_eq!(users[0]["id"].as_str(), node.id.as_deref());
    }

    fn check_ws_sets(ws: &Value, node: &Node) {
        assert_eq!(ws["path"].as_str(), node.path.as_deref());
        assert_eq!(ws["headers"]["Host"].as_str(), node.host.as_deref());
    }

    fn get_config_string() -> String {
        r#"{
            "log": {
                "loglevel": "debug"
            },
            "inbound": {
                "settings": {
                    "timeout": 5,
                    "allowTransparent": false,
                    "userLevel": 0
                },
                "protocol": "http",
                "port": 1080,
                "sniffing": {
                    "enabled": true,
                    "destOverride": [
                        "http",
                        "tls"
                    ]
                },
                "listen": "127.0.0.1"
            },
            "outbound": {
                "settings": {
                    "vnext": [
                        {
                            "address": "gz02.mobile.lay168.net",
                            "port": 61033,
                            "users": [
                                {
                                    "id": "55fb0457-d874-32c3-89a2-679fed6eabf1",
                                    "alterId": 2
                                }
                            ]
                        }
                    ]
                },
                "protocol": "vmess",
                "streamSettings": {
                    "wsSettings": {
                        "path": "/hls",
                        "headers": {
                            "Host": "hkt01.pqs-vds.lay168.net"
                        }
                    },
                    "network": "ws"
                }
            }
        }"#
        .to_string()
    }

    fn get_node() -> Node {
        serde_json::from_str(
            r#"{
            "host": "hk02.az.jinkela.icu",
            "path": "/hls",
            "tls": "",
            "verify_cert": true,
            "add": "gz01.mobile.lay168.net",
            "port": 61022,
            "aid": 2,
            "net": "ws",
            "headerType": "none",
            "localserver": "hk02.az.jinkela.icu",
            "v": "2",
            "type": "vmess",
            "ps": "广州01→香港02 | 1.5x NF",
            "remark": "广州01→香港02 | 1.5x NF",
            "id": "55fb0457-d874-32c3-89a2-679fed6eabf1",
            "class": 1
        }"#,
        )
        .unwrap()
    }
}

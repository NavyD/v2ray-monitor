use crate::node::Node;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

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
pub fn apply_config(contents: &str, nodes: &[&Node], local_port: Option<u16>) -> Result<String> {
    let mut contents = serde_json::from_str(contents)?;

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
    apply(&mut contents, "outbound.settings.vnext", json!(vnext))?;

    let node = nodes[0];
    apply(
        &mut contents,
        "outbound.streamSettings.wsSettings.path",
        json!(node.path.as_ref().expect("not found path")),
    )?;

    apply(
        &mut contents,
        "outbound.streamSettings.wsSettings.headers.host",
        json!(node.host.as_ref().expect("not found host")),
    )?;

    if let Some(port) = local_port {
        apply(&mut contents, "inbound.port", json!(port))?;
    }

    Ok(contents.to_string())
}

/// 使用path .分离出key找出对应的value并重置为new_val
fn apply(contents: &mut Value, path: &str, new_val: Value) -> Result<()> {
    let old = path
        .split('.')
        .fold(Some(contents), |val, key| val.and_then(|v| v.get_mut(key)))
        .ok_or_else(|| anyhow!("read config error: {}", path))?;
    log::trace!("old {}: {:?}", path, old);
    *old = new_val;
    log::trace!("applied {}: {:?}", path, old);
    Ok(())
}

/// 检查nodes是否符合配置要求
fn check_load_balance_nodes(nodes: &[&Node]) -> Result<()> {
    if nodes.is_empty() {
        return Err(anyhow!("empty nodes"));
    }
    // check nodes
    let host = nodes[0].host.as_deref();
    for node in nodes {
        // check node net type
        if !(node.net.as_deref() == Some("ws") && node.host.as_deref() == host) {
            log::error!(
                "filtered unsupported node: {:?}. by net type: {:?}, or host: {:?}",
                node.remark,
                node.net,
                node.host
            );
            return Err(anyhow!("unsupported config node: {:?}", node));
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
                        "host": "hk02.az.jinkela.icu"
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
        assert_eq!(ws["headers"]["host"].as_str(), node.host.as_deref());
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
                            "host": "hkt01.pqs-vds.lay168.net"
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

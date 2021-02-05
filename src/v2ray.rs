use std::{
    borrow::{Borrow},
    env::{split_paths, var_os},
    ops::{Deref, Range},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use async_tls::TlsConnector;

use crate::node::Node;

use serde_json::json;
use smol::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    lock::Mutex,
    net::TcpStream,
    process::Child,
};
use smol::{lock::Semaphore, stream::StreamExt};

pub struct V2rayProperty {
    pub bin_path: String,
    pub config_path: Option<String>,
    pub concurr_num: usize,
}

impl Default for V2rayProperty {
    /// 设置bin path为PATH中的v2ra，，config置为Non，，concurr_num
    /// 设为cpu_nums
    ///
    /// # panic
    ///
    /// 如果未在PATH中找到v2ray
    fn default() -> Self {
        let exe_name = "v2ray";
        let bin_path = var_os("PATH")
            .and_then(|val| {
                log::debug!("env path: {:?}", val.to_str());
                split_paths(&val).find_map(|path| {
                    if path.is_file() && path.ends_with(exe_name) {
                        return Some(path);
                    }
                    let path = path.join(exe_name);
                    if path.is_file() {
                        return Some(path);
                    }
                    None
                })
            })
            .and_then(|path| path.to_str().map(|s| s.to_owned()))
            .unwrap_or_else(|| panic!("not found v2ray in env var PATH"));
        log::debug!("found v2ray bin path: {}", bin_path);

        Self {
            bin_path,
            config_path: None,
            concurr_num: num_cpus::get(),
        }
    }
}

pub struct PingProperty {
    pub count: u8,
    pub max_port: u16,
    pub min_port: u16,
    pub ping_url: String,
    pub timeout: Duration,
}

impl Default for PingProperty {
    fn default() -> Self {
        PingProperty {
            count: 3,
            max_port: 60000,
            min_port: 50000,
            ping_url: "https://www.google.com/gen_204".into(),
            timeout: Duration::from_secs(3),
        }
    }
}

#[derive(Debug)]
pub struct TcpPingStatistic {
    durations: Vec<Option<Duration>>,
}

impl TcpPingStatistic {
    pub fn new(durations: Vec<Option<Duration>>) -> Self {
        Self { durations }
    }
}

#[derive(Clone)]
pub struct V2ray {
    inner: Arc<V2rayRef>,
}

impl V2ray {
    pub fn new(pp: PingProperty, vp: V2rayProperty) -> Self {
        Self {
            inner: Arc::new(V2rayRef::new(pp, vp)),
        }
    }
}

impl Deref for V2ray {
    type Target = V2rayRef;

    fn deref(&self) -> &Self::Target {
        self.inner.borrow()
    }
}

pub struct V2rayRef {
    local_ports: Mutex<Range<u16>>,
    ping_property: PingProperty,
    v2ray_property: V2rayProperty,
    // 限制v2ray并发启动个数
    semaphore: Semaphore,
}

impl V2rayRef {
    pub fn new(pp: PingProperty, vp: V2rayProperty) -> Self {
        V2rayRef {
            local_ports: Mutex::new(pp.min_port..pp.max_port),
            semaphore: Semaphore::new(vp.concurr_num),
            ping_property: pp,
            v2ray_property: vp,
        }
    }

    /// 加载配置config启动v2ray并返回v2ray子进程
    pub async fn start(&self, config: &str) -> Result<Child> {
        let mut child = smol::process::Command::new(&self.v2ray_property.bin_path)
            .arg("-config")
            .arg("stdin:")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        // 写完stdin后drop避免阻塞
        {
            child
                .stdin
                .take()
                .expect("stdin get error")
                .write_all(config.as_bytes())
                .await?;
        }

        // 2. start v2ray
        // take(): error trying to connect: Connection reset by peer (os error 104)
        let out = child.stdout.as_mut().expect("not found child stdout");
        let mut reader = BufReader::new(out).lines();
        while let Some(line) = reader.next().await {
            let line = line.expect("read line error");
            log::debug!("v2ray stdout line: {}", line);
            if line.contains("started") && line.contains("v2ray.com/core: V2Ray") {
                break;
            }
        }
        log::debug!("v2ray start successful");
        Ok(child)
    }

    /// 应用node到配置中启动v2ray并执行tcp ping
    pub async fn tcp_ping(&self, node: &Node) -> Result<TcpPingStatistic> {
        log::debug!("waiting for semaphore");
        let _guard = self.semaphore.acquire().await;
        log::info!("acquired semaphore: {:?}", _guard);

        // 0. load config
        let local_port = self.next_local_port().await;
        let config = gen_tcp_ping_config(node, local_port);

        // 1. start v2ray and hold on
        let mut _child = self.start(&config).await?;

        // print v2ray output for debug
        if log::log_enabled!(log::Level::Debug) {
            let out = _child.stdout.take().unwrap();
            let mut reader = BufReader::new(out).lines();
            smol::spawn(async move {
                while let Some(line) = reader.next().await {
                    log::debug!("ping v2ray line: {}", line.expect("read line error"));
                }
            })
            .detach();
        }

        // 3. send request to check
        let count = self.ping_property.count;
        let url = &self.ping_property.ping_url;
        let mut durations: Vec<Option<Duration>> = vec![None; count as usize];

        let (tx, rx) = smol::channel::bounded(1);
        for i in 0..count {
            let url = url.clone();
            let tx = tx.clone();
            smol::spawn(async move {
                let duration = measure_duration_with_proxy_run(&url, local_port)
                    .await
                    .map(Some)
                    .unwrap_or_else(|e| {
                        log::debug!("not found duration: {}", e);
                        None
                    });
                tx.send((i, duration)).await.unwrap();
            })
            .detach();
        }

        log::debug!("waiting for measure duration tasks: {}", count);
        for _ in 0..count {
            let (i, du) = rx.recv().await.unwrap();
            log::debug!("received task: ({}, {:?})", i, du);
            durations[i as usize] = du;
        }
        Ok(TcpPingStatistic::new(durations))
    }

    pub async fn next_local_port(&self) -> u16 {
        let mut ports = self.local_ports.lock().await;
        if let Some(port) = ports.next() {
            return port;
        }
        *ports = self.ping_property.min_port..self.ping_property.max_port;
        ports.next().unwrap()
    }
}

use async_h1::client;
use http_types::Version;
use http_types::{Method, Request, Url};

async fn measure_duration_with_proxy_run(url: &str, proxy_port: u16) -> anyhow::Result<Duration> {
    log::debug!(
        "sending get request url: {},  with localhost socks5 proxy port: {}",
        url,
        proxy_port
    );

    let tcp_stream = TcpStream::connect(format!("127.0.0.1:{}", proxy_port)).await?;
    let url = Url::parse(url)?;

    let mut req = Request::new(Method::Connect, url.clone());
    req.set_version(Some(Version::Http1_1));
    req.insert_header("User-Agent", "curl/7.69.1");

    let resp = client::connect(tcp_stream.clone(), req).await.unwrap();
    assert!(resp.status().is_success());

    // let s = TcpStream::connect("google.com:80").await?;

    let mut req = Request::new(Method::Get, url);
    req.set_version(Some(Version::Http1_1));
    req.insert_header("User-Agent", "curl/7.69.1");
    req.insert_header("Accept", "*/*");

    let start = Instant::now();
    let resp = match req.url().scheme() {
        "http" => client::connect(tcp_stream, req).await,
        "https" => {
            let stream = TlsConnector::default()
                .connect(req.host().unwrap(), tcp_stream)
                .await?;
            client::connect(stream, req).await
        }
        _ => panic!("unsupported for proxy url scheme: {}", req.url()),
    }
    .map_err(|e| e.into_inner())?;

    let duration = Instant::now() - start;
    let status = resp.status();
    if !status.is_success() {
        log::warn!("error status: {}", status);
    }
    Ok(duration)
}

/// 应用nodes生成v2ray负载均衡配置
///
/// # panic
///
/// * 如果nodes为空
/// * node.host不一致时
/// * node.net不是`ws`类型时
/// * node关键字段中存在None
fn gen_load_balance_config(nodes: &[&Node], local_port: u16) -> String {
    if nodes.is_empty() {
        panic!("nodes is empty");
    }
    // check nodes
    {
        let host = nodes[0].host.as_deref();
        let discord_nodes = nodes
            .iter()
            .map(|node| {
                // check node net type
                match node.net.as_deref() {
                    Some("ws") => node.host.as_deref(),
                    _ => panic!("unsupported node net type: {:?}", node.net),
                }
            })
            .filter(|other| other != &host)
            .collect::<Vec<_>>();
        if !discord_nodes.is_empty() {
            log::error!(
                "There are nodes with inconsistent hosts: {:?}, host: {:?}",
                discord_nodes,
                host
            );
            panic!("inconsistent hosts: {:?}", nodes);
        }
    }
    let next_items = nodes
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

    let node = &nodes[0];
    json!({
        "log": {
            "loglevel": "debug"
        },
        "inbound": {
            "settings": {
                "ip": "127.0.0.1"
            },
            "protocol": "socks",
            "port": local_port,
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
                "vnext": next_items
            },
            "protocol": "vmess",
            "streamSettings": {
                "wsSettings": {
                    "path": node.path.as_ref().expect("not found path"),
                    "headers": {
                        "host": node.host.as_ref().expect("not found host")
                    }
                },
                "network": "ws"
            }
        }
    })
    .to_string()
}

fn gen_tcp_ping_config(node: &Node, local_port: u16) -> String {
    json!({
        "log": {
            "loglevel": "debug"
        },
        "inbound": {
            "settings": {
                "timeout": 0,
                "allowTransparent": false,
                "userLevel": 0
            },
            "protocol": "http",
            "port": local_port,
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
                        "address": node.add.as_ref().expect("not found address"),
                        "port": node.port.as_ref().expect("not found port"),
                        "users": [
                            {
                                "id": node.id.as_ref().expect("not found id"),
                                "alterId": node.aid.as_ref().expect("not found alter_id")
                            }
                        ]
                    }
                ]
            },
            "protocol": "vmess",
            "streamSettings": {
                "wsSettings": {
                    "path": node.path.as_ref().expect("not found path"),
                    "headers": {
                        "host": node.host.as_ref().expect("not found host")
                    }
                },
                "network": "ws"
            }
        }
    })
    .to_string()
}

#[cfg(test)]
mod v2ray_tests {

    use std::sync::Once;

    use super::*;
    use log::LevelFilter;
    use serde_json::Value;

    #[test]
    fn gen_config_nodes() -> Result<()> {
        let local_port = 1000;
        let config = gen_load_balance_config(&[&get_node(), &get_node()], local_port);
        log::debug!("config: {}", config);
        let node = get_node();
        assert!(!config.is_empty());

        let val: Value = serde_json::from_str(&config)?;
        let vnext_items = &val["outbound"]["settings"]["vnext"].as_array().unwrap();
        assert_eq!(vnext_items.len(), 2);

        for i in 0..vnext_items.len() {
            assert_eq!(vnext_items[i]["address"].as_str(), node.add.as_deref());
            assert_eq!(vnext_items[i]["port"].as_u64(), node.port.map(|p| p as u64));

            let users = &vnext_items[i]["users"];
            assert_eq!(users.as_array().map(|a| a.len()), Some(1));
            assert_eq!(users[0]["alterId"].as_u64(), node.aid.map(|a| a as u64));
            assert_eq!(users[0]["id"].as_str(), node.id.as_deref());
        }
        Ok(())
    }

    #[test]
    fn start_test() {
        async fn test() -> Result<()> {
            let node = get_node();
            let v2ray = get_v2ray();
            let config = gen_tcp_ping_config(&node, v2ray.next_local_port().await);
            v2ray.start(&config).await?;
            Ok(())
        }
        smol::block_on(async {
            test().await.unwrap();
        });
    }

    #[test]
    fn measure_duration_with_v2ray_start() {
        async fn test() -> Result<()> {
            let node = get_node();
            let v2ray = get_v2ray();
            let local_port = v2ray.next_local_port().await;

            let config = gen_tcp_ping_config(&node, local_port);
            let mut child = v2ray.start(&config).await?;

            futures::executor::block_on(async_compat::Compat::new(async {
                measure_duration_with_proxy_run(&v2ray.ping_property.ping_url, local_port)
                    .await
                    .expect("get duration error");
            }));
            child.kill()?;
            Ok(())
        }
        smol::block_on(async {
            test().await.unwrap();
        });
    }

    #[test]
    fn tcp_ping_test() {
        async fn test() -> Result<()> {
            let node = get_node();
            let v2ray = get_v2ray();
            let stats = v2ray.tcp_ping(&node).await?;
            assert_eq!(
                stats.durations.len(),
                PingProperty::default().count as usize
            );
            assert!(stats.durations.iter().filter(|d| d.is_some()).count() > 0);
            Ok(())
        }
        smol::block_on(async {
            test().await.unwrap();
        });
    }

    #[test]
    fn tcp_ping_error_when_node_unavailable() {
        async fn test() -> Result<()> {
            let mut node = get_node();
            node.add = Some("test.host.addr".to_owned());
            // node.host = Some("2423".to_owned());
            let v2ray = get_v2ray();
            let stats = v2ray.tcp_ping(&node).await?;
            assert_eq!(
                stats.durations.len(),
                PingProperty::default().count as usize
            );
            assert_eq!(stats.durations.iter().filter(|d| d.is_some()).count(), 0);
            Ok(())
        }
        smol::block_on(async {
            test().await.unwrap();
        });
    }

    static INIT: Once = Once::new();

    #[cfg(test)]
    #[ctor::ctor]
    fn init() {
        INIT.call_once(|| {
            env_logger::builder()
                .is_test(true)
                .filter_level(LevelFilter::Debug)
                .init();
        });
    }

    fn get_v2ray() -> V2rayRef {
        V2rayRef::new(
            PingProperty::default(),
            V2rayProperty {
                config_path: Some("/home/navyd/Downloads/v2ray/v2-config-test.json".to_owned()),
                ..Default::default()
            },
        )
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

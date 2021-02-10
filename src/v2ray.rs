use std::{
    borrow::{Borrow, BorrowMut},
    cmp::Ordering,
    env::{split_paths, var_os},
    ops::{Deref, DerefMut},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::config::*;
use crate::node::Node;
use crate::task::v2ray_task_config::*;

use anyhow::Result;

use reqwest::Proxy;
use tokio::{
    fs::read_to_string,
    io::*,
    net::TcpListener,
    process::{Child, Command},
    runtime::Runtime,
    sync::{mpsc::channel, Mutex, Semaphore},
    task::spawn_blocking,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Eq, Clone)]
pub struct TcpPingStatistic {
    pub durations: Vec<Option<Duration>>,
    pub count: usize,
    pub received_count: usize,
    pub rtt_min: Option<Duration>,
    pub rtt_max: Option<Duration>,
    pub rtt_avg: Option<Duration>,
}

impl PartialEq for TcpPingStatistic {
    fn eq(&self, other: &Self) -> bool {
        self.rtt_avg == other.rtt_avg
            && self.rtt_min == other.rtt_min
            && self.rtt_max == other.rtt_max
    }
}

impl Ord for TcpPingStatistic {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let compare = |a: Option<Duration>, b: Option<Duration>| match (a, b) {
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (a, b) => a.cmp(&b),
        };
        let v = compare(self.rtt_avg, other.rtt_avg);
        if v != Ordering::Equal {
            return v;
        }
        let v = compare(self.rtt_min, other.rtt_min);
        if v != Ordering::Equal {
            return v;
        }
        compare(self.rtt_max, other.rtt_max)
    }
}

impl PartialOrd for TcpPingStatistic {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(&other))
    }
}

impl TcpPingStatistic {
    pub fn new(durations: Vec<Option<Duration>>) -> Self {
        let mut received_count = 0;
        let mut rtt_min = None;
        let mut rtt_max = None;
        let mut total = Duration::from_nanos(0);
        for d in &durations {
            let d = *d;
            if let Some(d) = d {
                received_count += 1;
                total += d;
                if rtt_min.is_none() {
                    rtt_min = Some(d);
                }
            }
            if rtt_min > d {
                rtt_min = d;
            }
            if rtt_max < d {
                rtt_max = d;
            }
        }
        let rtt_avg: Option<Duration> = if received_count != 0 {
            Some(total / received_count)
        } else {
            None
        };
        Self {
            count: durations.len(),
            durations,
            received_count: received_count as usize,
            rtt_avg,
            rtt_max,
            rtt_min,
        }
    }

    pub fn is_accessible(&self) -> bool {
        self.received_count > 0
    }
}

#[derive(Clone)]
pub struct V2ray {
    inner: Arc<V2rayRef>,
}

impl V2ray {
    pub fn new(property: V2rayProperty) -> Self {
        Self {
            inner: Arc::new(V2rayRef::new(property)),
        }
    }
}

impl Deref for V2ray {
    type Target = V2rayRef;

    fn deref(&self) -> &Self::Target {
        self.inner.borrow()
    }
}

impl DerefMut for V2ray {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.borrow_mut()
    }
}

pub struct V2rayRef {
    property: V2rayProperty,
    v2_child: Mutex<Option<Child>>,
    bin_path: String,
    concurr_num: usize,
}

impl V2rayRef {
    pub fn new(property: V2rayProperty) -> Self {
        let bin_path = property
            .bin_path
            .clone()
            .unwrap_or_else(|| find_bin_path("v2ray").expect("not found v2ray bin path"));
        let concurr_num = property.concurr_num.unwrap_or_else(num_cpus::get);
        Self {
            property,
            v2_child: Mutex::new(None),
            bin_path,
            concurr_num,
        }
    }

    pub async fn restart_load_balance(&self, nodes: &[&Node]) -> Result<()> {
        if let Some(mut child) = self.v2_child.lock().await.take() {
            log::debug!("killing old v2ray proccess: {:?}", child.id());
            child.kill().await?;
        } else {
            log::debug!("killing all v2ray proccess");
            killall_v2ray().await?;
        }
        let mut child = self.v2_child.lock().await;
        *child = Some(self.start_load_balance(nodes).await?);
        Ok(())
    }

    /// 使用负载均衡配置启动v2ray。如果`V2rayProperty::config_path`为空则使用默认的配置
    async fn start_load_balance(&self, nodes: &[&Node]) -> Result<Child> {
        let vp = &self.property;
        let config = if let Some(path) = &vp.config_path {
            let contents = read_to_string(path).await?;
            apply_config(&contents, nodes, None)?
        } else {
            gen_load_balance_config(
                nodes,
                vp.port.expect("not found port in gen_load_balance_config"),
            )?
        };
        start(&self.bin_path, &config).await
    }

    /// ping nodes并返回统计数据
    ///
    /// 用户可使用[`V2rayProperty::concurr_num`]控制并发v2ray数，但对系统有内存要求
    pub async fn tcp_ping_nodes(
        &self,
        nodes: Vec<Node>,
        ping_property: &PingProperty,
    ) -> Vec<(Node, TcpPingStatistic)> {
        let size = nodes.len();
        let mut res = vec![];
        log::debug!("start tcp ping {} nodes", size);

        let (tx, mut rx) = channel(1);
        let semaphore = Arc::new(Semaphore::new(self.concurr_num));
        let start = Instant::now();

        for node in nodes {
            let bin_path = self.bin_path.clone();
            let pp = ping_property.clone();
            let semaphore = semaphore.clone();
            let tx = tx.clone();
            let port = get_available_port()
                .await
                .expect("not found available port");

            tokio::spawn(async move {
                let _permit = semaphore.acquire().await.unwrap();
                let config = gen_tcp_ping_config(&node, port).unwrap_or_else(|e| {
                    panic!(
                        "generate tcp ping config error: {}, with node: {:?}",
                        e, node
                    )
                });
                let ps = tcp_ping(&bin_path, &config, port, &pp).await;
                log::debug!(
                    "got tcp ping statistic. node: {:?}, ps: {:?}",
                    node.remark,
                    ps
                );
                tx.send((node, ps)).await.unwrap();
            });
        }

        // drop(tx);

        while let Some((node, ps)) = rx.recv().await {
            if let Err(e) = ps {
                log::info!(
                    "received error tcp ping node: {:?}, ping statistic: {}",
                    node.remark,
                    e
                );
            } else {
                res.push((node, ps.unwrap()));
            }
        }
        let perform_duration = Instant::now() - start;
        log::debug!("tcp ping {} nodes takes {:?}", size, perform_duration);
        res
    }
}

fn find_bin_path(name: &str) -> Result<String> {
    let exe_name = "v2ray";
    var_os("PATH")
        .and_then(|val| {
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
        .ok_or_else(|| anyhow::anyhow!("not found {} in env var PATH", name))
}

async fn killall_v2ray() -> Result<()> {
    let out = Command::new("killall")
        .arg("-9")
        .arg("v2ray")
        .output()
        .await?;
    if out.status.success() {
        log::debug!(
            "killall v2ray success: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    } else {
        log::debug!(
            "killall v2ray failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// 从bin path中使用config启动v2ray 并返回子进程 由用户控制
async fn start(bin_path: &str, config: &str) -> Result<Child> {
    let mut child = Command::new(bin_path)
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

    // 2. check start with output
    // 不能使用stdout.take(): error trying to connect: Connection reset by peer (os error 104)
    let out = child.stdout.as_mut().expect("not found child stdout");
    let mut reader = BufReader::new(out).lines();
    while let Some(line) = reader.next_line().await? {
        log::debug!("v2ray stdout line: {}", line);
        if line.contains("started") && line.contains("v2ray.com/core: V2Ray") {
            break;
        }
    }
    log::debug!("v2ray start successful");
    Ok(child)
}

async fn get_available_port() -> Result<u16> {
    let listen = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listen.local_addr()?;
    Ok(addr.port())
}

/// 应用配置启动v2ray并执行tcp ping
///
/// config中的port必须与local_port一样
async fn tcp_ping(
    bin_path: &str,
    config: &str,
    local_port: u16,
    ping_property: &PingProperty,
) -> Result<TcpPingStatistic> {
    // 1. start v2ray and hold on
    let mut _child = start(bin_path, config).await?;

    // print v2ray output for Trace
    if log::log_enabled!(log::Level::Trace) {
        let out = _child.stdout.take().unwrap();
        let mut reader = BufReader::new(out).lines();
        tokio::spawn(async move {
            while let Some(line) = reader.next_line().await.unwrap_or_else(|e| panic!("{}", e)) {
                log::trace!("ping v2ray line: {}", line);
            }
        });
    }

    // 2. send request
    let count = ping_property.count;
    let url = ping_property.ping_url.to_owned();
    let timeout = ping_property.timeout;
    let mut durations: Vec<Option<Duration>> = vec![None; count as usize];

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    for i in 0..count {
        let url = url.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let idx_du = measure_duration_with_proxy(&url, local_port, timeout)
                .await
                .map(|d| (i, Some(d)))
                .unwrap_or_else(|e| {
                    log::trace!("not found duration: {}", e);
                    (i, None)
                });
            tx.send(idx_du)
                .await
                .unwrap_or_else(|e| panic!("send on {:?} error: {}", idx_du, e));
        });
    }

    drop(tx);

    log::debug!("waiting for measure duration {} tasks", count);
    while let Some((i, du)) = rx.recv().await {
        log::trace!("received task result: ({}, {:?})", i, du);
        durations[i as usize] = du;
    }
    Ok(TcpPingStatistic::new(durations))
}

/// 使用本地v2ray port测量http get url的时间
async fn measure_duration_with_proxy(
    url: &str,
    proxy_port: u16,
    timeout: Duration,
) -> reqwest::Result<Duration> {
    log::trace!(
        "sending get request url: {},  with localhost socks5 proxy port: {}",
        url,
        proxy_port
    );

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .proxy(Proxy::https(&format!("socks5://127.0.0.1:{}", proxy_port))?)
        .build()?;

    let start = Instant::now();
    let status = client.get(url).send().await?.status();
    let duration = Instant::now() - start;

    if status.as_u16() >= 400 {
        log::info!("request {} has error status: {}", url, status);
    }
    log::trace!("request {} has duration: {:?}", url, duration);
    Ok(duration)
}

#[cfg(test)]
mod tests {

    use std::sync::Once;

    use super::*;
    use log::LevelFilter;

    #[cfg(test)]
    mod v2ray_tests {
        use super::*;

        #[tokio::test]
        async fn tcp_ping() -> Result<()> {
            let v2 = V2ray::new(Default::default());
            let pp = Default::default();
            let nodes = vec![get_node(), get_node(), get_node()];
            let ps = v2.tcp_ping_nodes(nodes, &pp).await;
            log::debug!("{:?}", ps);
            Ok(())
        }
    }

    #[tokio::test]
    async fn start_test() -> Result<()> {
        let node = get_node();
        let vp = V2rayProperty::default();
        let config = gen_tcp_ping_config(&node, get_available_port().await?)?;
        let bin_path = vp
            .bin_path
            .unwrap_or_else(|| find_bin_path("v2ray").unwrap());

        start(&bin_path, &config).await?;
        Ok(())
    }

    // #[test]
    #[tokio::test]
    async fn measure_duration_with_v2ray_start() -> Result<()> {
        let node = get_node();
        let vp = V2rayProperty::default();
        let pp = PingProperty::default();

        let local_port = get_available_port().await?;
        let config = gen_tcp_ping_config(&node, local_port)?;
        let bin_path = vp
            .bin_path
            .unwrap_or_else(|| find_bin_path("v2ray").unwrap());

        let mut child = start(&bin_path, &config).await?;

        measure_duration_with_proxy(&pp.ping_url, local_port, Duration::from_secs(2))
            .await
            .expect("get duration error");
        child.kill().await?;
        Ok(())
    }

    #[tokio::test]
    async fn tcp_ping_test() -> Result<()> {
        let node = get_node();
        let vp = V2rayProperty::default();
        let pp = PingProperty::default();
        let local_port = get_available_port().await?;
        let config = gen_tcp_ping_config(&node, local_port)?;
        let bin_path = vp
            .bin_path
            .unwrap_or_else(|| find_bin_path("v2ray").unwrap());

        let stats = tcp_ping(&bin_path, &config, local_port, &pp).await?;
        assert_eq!(
            stats.durations.len(),
            PingProperty::default().count as usize
        );
        assert!(stats.durations.iter().filter(|d| d.is_some()).count() > 0);
        Ok(())
    }

    #[tokio::test]
    async fn tcp_ping_error_when_node_unavailable() -> Result<()> {
        let mut node = get_node();
        node.add = Some("test.host.addr".to_owned());

        let vp = V2rayProperty::default();
        let pp = PingProperty::default();
        let local_port = get_available_port().await?;
        let config = gen_tcp_ping_config(&node, local_port)?;
        let bin_path = vp
            .bin_path
            .unwrap_or_else(|| find_bin_path("v2ray").unwrap());

        let stats = tcp_ping(&bin_path, &config, local_port, &pp).await?;

        assert_eq!(
            stats.durations.len(),
            PingProperty::default().count as usize
        );
        assert_eq!(stats.durations.iter().filter(|d| d.is_some()).count(), 0);
        Ok(())
    }

    static INIT: Once = Once::new();

    #[cfg(test)]
    #[ctor::ctor]
    fn init() {
        INIT.call_once(|| {
            env_logger::builder()
                .is_test(true)
                .filter_level(LevelFilter::Debug)
                .filter_module("reqwest", LevelFilter::Debug)
                .init();
        });
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

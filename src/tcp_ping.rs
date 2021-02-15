use crate::v2ray::{ConfigurableV2ray, V2rayService};
use crate::{task::v2ray_task_config::*, v2ray::node::Node};

use std::{
    borrow::{Borrow, BorrowMut},
    cmp::Ordering,
    env::{split_paths, var_os},
    ops::{Deref, DerefMut},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};

use async_trait::async_trait;
use parking_lot::Mutex;
use reqwest::{Client, Proxy};
use tokio::{
    fs::read_to_string,
    io::*,
    net::TcpListener,
    process::{Child, Command},
    sync::{mpsc::channel, Semaphore},
};

// #[async_trait]
// pub trait TcpPingService: Send + Sync {
//     async fn ping(&self, node: &Node) -> Result<TcpPingStatistic>;

//     async fn ping_batch<'a>(
//         &self,
//         nodes: &[&'a Node],
//     ) -> Result<(Vec<(&'a Node, TcpPingStatistic)>, Option<Vec<&'a Node>>)>;
// }

// pub struct TcpPingServiceImpl<V: V2rayService + ConfigurableV2ray> {
//     ports: Arc<Mutex<Vec<u16>>>,
//     prop: Arc<PingProperty>,
//     v2: V,
// }

// impl<V: V2rayService + ConfigurableV2ray> TcpPingServiceImpl<V> {

// }

// #[async_trait]
// impl<V: V2rayService + ConfigurableV2ray> TcpPingService for TcpPingServiceImpl<V> {
//     async fn ping(&self, node: &Node) -> Result<TcpPingStatistic> {
//         todo!()
//     }

//     async fn ping_batch<'a>(
//         &self,
//         nodes: &[&'a Node],
//     ) -> Result<(Vec<(&'a Node, TcpPingStatistic)>, Option<Vec<&'a Node>>)> {
//         todo!()
//     }
// }

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

use crate::v2ray::config;

/// 应用配置启动v2ray并执行tcp ping。当函数完成时v2ray将被killed
async fn tcp_ping<T: V2rayService>(
    service: T,
    node: &Node,
    prop: &PingProperty,
) -> Result<TcpPingStatistic> {
    let port = service.get_available_port().await?;
    let config = service.gen_ping_config(&[node], port).await?;
    log::trace!(
        "generated node: {:?}, port: {}, config: {}",
        node,
        port,
        config
    );
    // 1. start v2ray and hold on
    let mut _child = service.start(&config).await?;

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

    let (url, count, timeout) = (prop.ping_url.clone(), prop.count, prop.timeout);
    let mut durations: Vec<Option<Duration>> = vec![None; count as usize];

    let (tx, mut rx) = channel(1);
    for i in 0..count {
        let url = url.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let idx_du = measure_duration_with_proxy(&url, port, timeout)
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

    log::trace!("waiting for measure duration {} tasks", count);
    while let Some((i, du)) = rx.recv().await {
        log::trace!("received task result: ({}, {:?})", i, du);
        durations[i as usize] = du;
    }
    Ok(TcpPingStatistic::new(durations))
}

async fn ping_task(prop: &PingProperty, proxy_url: &str) -> Result<TcpPingStatistic> {
    let (url, count, timeout) = (prop.ping_url.clone(), prop.count, prop.timeout);
    let mut durations: Vec<Option<Duration>> = vec![None; count as usize];
    let (tx, mut rx) = channel(count.into());

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .proxy(Proxy::all(proxy_url)?)
        .build()?;

    async fn compute_duration(client: &Client, url: &str) -> Result<Duration> {
        log::trace!("sending get request url: `{}`", url);
        let start = Instant::now();
        let status = client.get(url).send().await?.status();
        let duration = Instant::now() - start;
        if status.as_u16() >= 400 {
            log::info!("request {} has error status: {}", url, status);
        }
        log::trace!("request {} has duration: {:?}", url, duration);
        Ok(duration)
    }

    for i in 0..count {
        let url = url.clone();
        let tx = tx.clone();
        let client = client.clone();
        tokio::spawn(async move {
            let idx_du = compute_duration(&client, &url)
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

    log::trace!("waiting for measure duration {} tasks", count);
    while let Some((i, du)) = rx.recv().await {
        log::trace!("received task result: ({}, {:?})", i, du);
        durations[i as usize] = du;
    }
    Ok(TcpPingStatistic::new(durations))
}

async fn ping_batch<'a, T: V2rayService + 'static>(
    v2: T,
    nodes: Vec<Node>,
    prop: &PingProperty,
) -> Result<(Vec<(&'a Node, TcpPingStatistic)>, Option<Vec<&'a Node>>)> {
    if nodes.is_empty() {
        return Err(anyhow!("nodes is empty"));
    }
    let size = nodes.len();
    log::debug!("starting tcp ping {} nodes", size);
    let ports = v2.get_running_ports();
    if ports.is_none() {
        return Err(anyhow!("not found running ports in v2 service"));
    }

    let (tx, mut rx) = channel(size);
    let semaphore = Arc::new(Semaphore::new(prop.concurr_num));
    let start = Instant::now();
    for node in nodes {
        let (prop, semaphore, tx, service) =
            (prop.clone(), semaphore.clone(), tx.clone(), v2.clone());
        tokio::spawn(async move {
            let _permit = semaphore.acquire().await.unwrap();
            let ps = ping_task(&prop, proxy_url);
            tx.send((node, ps)).await.unwrap();
        });
    }

    drop(tx);

    while let Some((node, ps)) = rx.recv().await {
        if let Err(e) = ps {
            log::warn!(
                "ignored node name: {:?}, address: {:?}, for received error tcp ping: {}",
                node.remark,
                node.add,
                e
            );
        } else {
            res.push((node, ps.unwrap()));
        }
    }
    let perform_duration = Instant::now() - start;
    log::debug!("tcp ping {} nodes takes {:?}", size, perform_duration);
    todo!()
}

/// ping nodes并返回统计数据
///
/// 用户可使用[`PingProperty::concurr_num`]控制并发v2ray数，但对系统有内存要求
///
/// 如果没有找到对应可用的端口则跳过这个node统计
pub async fn tcp_ping_nodes<T: V2rayService + 'static>(
    v2: T,
    nodes: Vec<Node>,
    prop: &PingProperty,
) -> Vec<(Node, TcpPingStatistic)> {
    let size = nodes.len();
    log::debug!("starting tcp ping {} nodes", size);
    let mut res = vec![];
    if nodes.is_empty() {
        return res;
    }

    let (tx, mut rx) = channel(1);
    let semaphore = Arc::new(Semaphore::new(prop.concurr_num));
    let start = Instant::now();
    for node in nodes {
        let (prop, semaphore, tx, service) =
            (prop.clone(), semaphore.clone(), tx.clone(), v2.clone());

        tokio::spawn(async move {
            let _permit = semaphore.acquire().await.unwrap();
            let ps = tcp_ping(service, &node, &prop).await;
            tx.send((node, ps)).await.unwrap();
        });
    }

    drop(tx);

    while let Some((node, ps)) = rx.recv().await {
        if let Err(e) = ps {
            log::warn!(
                "ignored node name: {:?}, address: {:?}, for received error tcp ping: {}",
                node.remark,
                node.add,
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
async fn compute_duration(
    url: &str,
    proxy_url: &str,
    timeout: Duration,
) -> reqwest::Result<Duration> {
    log::trace!("sending get request url: `{}` by proxy: {}", url, proxy_url);

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .proxy(Proxy::all(proxy_url)?)
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

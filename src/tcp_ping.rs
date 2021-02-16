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

use crate::v2ray::config;
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

pub async fn ping_batch11<'a, T: V2rayService + 'static>(
    v2: T,
    nodes: &[Node],
    prop: PingProperty,
) -> Result<(Vec<(&Node, TcpPingStatistic)>, Option<Vec<&Node>>)> {
    todo!()
}

/// 对nodes节点批量ping返回能ping通的节点与不可通的节点。
///
pub async fn ping_batch<'a, T: V2rayService + 'static>(
    v2: T,
    nodes: Vec<Node>,
    prop: &PingProperty,
) -> Result<(Vec<(Node, TcpPingStatistic)>, Option<Vec<Node>>)> {
    if nodes.is_empty() {
        return Err(anyhow!("nodes is empty"));
    }
    let size = nodes.len();
    let concurr_num = size.min(prop.concurr_num);
    log::debug!("Start {} v2ray to ping {} nodes", concurr_num, size);

    let (tx, mut rx) = channel(size);
    let semaphore = Arc::new(Semaphore::new(concurr_num));
    let start = Instant::now();
    for node in nodes {
        let (prop, semaphore, tx, v2) = (prop.clone(), semaphore.clone(), tx.clone(), v2.clone());
        tokio::spawn(async move {
            let ps = if let Err(e) = semaphore.acquire().await {
                log::error!(
                    "Unable to ping node {:?} because closed semaphore: {}",
                    node.remark,
                    e
                );
                Err(anyhow!("closed semaphore: {}", e))
            } else {
                ping_task(v2, &node, &prop).await
            };
            tx.send((node, ps)).await.expect("send error");
        });
    }

    drop(tx);

    let (mut res, mut err_nodes) = (vec![], None);
    while let Some((node, ps)) = rx.recv().await {
        match ps {
            Ok(ps) => res.push((node, ps)),
            Err(e) => {
                log::warn!(
                    "ignored node name: {:?}, address: {:?}, for received error tcp ping: {}",
                    node.remark,
                    node.add,
                    e
                );
                err_nodes.get_or_insert_with(Vec::new).push(node);
            }
        }
    }
    let exe_dura = Instant::now() - start;
    log::debug!(
        "tcp ping {} nodes takes {:?}.  accessible nodes: {}, error nodes: {}",
        size,
        exe_dura,
        res.len(),
        err_nodes.as_ref().map(|a| a.len()).unwrap_or(0)
    );
    // Ok((res, err_nodes))
    todo!()
}

async fn ping_task<T: V2rayService>(
    v2: T,
    node: &Node,
    prop: &PingProperty,
) -> Result<TcpPingStatistic> {
    let (url, count, timeout) = (prop.ping_url.clone(), prop.count, prop.timeout);
    let mut durations: Vec<Option<Duration>> = vec![None; count as usize];

    let port = v2.get_available_port().await?;
    let config = v2.gen_ping_config(node, port).await?;
    let proxy_url = v2.get_proxy_url(&config).await?;
    v2.start_in_background(&config).await?;

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .proxy(Proxy::all(&proxy_url)?)
        .build()?;

    let (tx, mut rx) = channel(count.into());
    log::trace!(
        "calculating the ping data of node: {:?}, addr: {:?}",
        node.remark,
        node.add
    );
    let start = Instant::now();
    for i in 0..count {
        let url = url.clone();
        let tx = tx.clone();
        let client = client.clone();
        tokio::spawn(async move {
            let idx_dura = calculate_duration(&client, &url)
                .await
                .map(|d| (i, Some(d)))
                .unwrap_or_else(|e| {
                    log::debug!("not found duration error: {}", e);
                    (i, None)
                });
            tx.send(idx_dura)
                .await
                .unwrap_or_else(|e| panic!("send on {:?} error: {}", idx_dura, e));
        });
    }

    drop(tx);

    log::trace!("waiting for measure duration {} tasks", count);
    while let Some((i, du)) = rx.recv().await {
        log::trace!("received task result: ({}, {:?})", i, du);
        durations[i as usize] = du;
    }
    let exe_dura = Instant::now() - start;
    log::debug!(
        "it takes {:?} to count the data of node: {:?}",
        exe_dura,
        node.remark
    );

    v2.stop(port).await?;
    let ps = TcpPingStatistic::new(durations);
    if !ps.is_accessible() {
        log::trace!(
            "node {:?} cannot be accessed within the timeout: {:?}",
            node.remark,
            timeout
        );
        Err(anyhow!("node {:?} cannot be accessed", node.remark))
    } else {
        Ok(ps)
    }
}

async fn calculate_duration(client: &Client, url: &str) -> Result<Duration> {
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::v2ray::LocalV2ray;
    use crate::{task::find_v2ray_bin_path, v2ray::node};

    use super::*;

    #[test]
    fn basic() {}

    #[tokio::test]
    async fn ping_test() -> Result<()> {
        let node = get_node();
        let v2 = LocalV2ray::new(get_local_prop()?);
        let ping_prop = get_ping_prop()?;

        let stats = ping_task(v2, &node, &ping_prop).await?;
        assert_eq!(stats.durations.len(), ping_prop.count as usize);
        assert!(stats.is_accessible());
        Ok(())
    }

    #[tokio::test]
    async fn ping_batch_one() -> Result<()> {
        let node = get_node();
        let nodes = vec![node];
        let old_len = nodes.len();
        let v2 = LocalV2ray::new(get_local_prop()?);
        let ping_prop = get_ping_prop()?;

        let (nps, err_nodes) = ping_batch(v2, nodes, &ping_prop).await?;
        assert!(err_nodes.is_none());
        assert_eq!(nps.len(), old_len);
        assert!(nps.iter().any(|(_, ps)| ps.is_accessible()));
        Ok(())
    }

    #[tokio::test]
    async fn ping_batch_from_more() -> Result<()> {
        let sub_path = "tests/data/v2ray-subscription.txt";
        let mut nodes = node::load_subscription_nodes_from_file(sub_path).await?;
        let nodes = nodes.drain(..).into_iter().collect::<Vec<_>>();
        let old_len = nodes.len();

        let v2 = LocalV2ray::new(get_local_prop()?);
        let mut ping_prop = get_ping_prop()?;
        ping_prop.concurr_num = old_len / 5;

        let (nps, err_nodes) = ping_batch(v2, nodes, &ping_prop).await?;
        if let Some(enodes) = err_nodes {
            assert_eq!(nps.len() + enodes.len(), old_len);
        } else {
            assert_eq!(nps.len(), old_len);
        }
        assert!(nps.iter().any(|(_, ps)| ps.is_accessible()));
        Ok(())
    }

    fn get_local_prop() -> Result<LocalV2rayProperty> {
        Ok(LocalV2rayProperty {
            bin_path: find_v2ray_bin_path()?,
            config_path: Some(
                Path::new("tests/data")
                    .join("local-v2-config.json")
                    .to_string_lossy()
                    .to_string(),
            ),
        })
    }

    fn get_ping_prop() -> Result<PingProperty> {
        let content = r#"
count: 3
ping_url: https://www.google.com/gen_204
timeout:
    secs: 1
    nanos: 5000000
#concurr_num: 50"#;
        Ok(serde_yaml::from_str::<PingProperty>(content)?)
    }

    fn get_ssh_prop() -> Result<SshV2rayProperty> {
        let content = r#"
username: root
host: 192.168.93.2
config_path: /var/etc/ssrplus/tcp-only-ssr-retcp.json
bin_path: /usr/bin/v2ray
"#;
        Ok(serde_yaml::from_str::<SshV2rayProperty>(content)?)
    }

    fn get_node() -> Node {
        serde_json::from_str(
            r#"{"host":"hkbn.vds.nbsd.us","path":"/hls","tls":"","verify_cert":true,"add":"iplc03.cncm.lay168.net","port":9021,"aid":2,"net":"ws","headerType":"none","v":"2","type":"none","ps":"粤港03 IEPL专线 入口3 | 4x NF","remark":"粤港03 IEPL专线 入口3 | 4x NF","id":"55fb0457-d874-32c3-89a2-679fed6eabf1","class":1}"#,
        )
        .unwrap()
    }
}

//     #[tokio::test]
//     async fn tcp_ping_error_when_node_unavailable() -> Result<()> {
//         let mut node = get_node();
//         node.add = Some("test.host.addr".to_owned());

//         let vp = V2rayProperty::default();
//         let pp = PingProperty::default();
//         let local_port = get_available_port().await?;
//         let config = gen_tcp_ping_config(&node, local_port)?;
//         let bin_path = vp
//             .bin_path
//             .unwrap_or_else(|| find_bin_path("v2ray").unwrap());

//         let stats = tcp_ping(&bin_path, &config, local_port, &pp).await?;

//         assert_eq!(
//             stats.durations.len(),
//             PingProperty::default().count as usize
//         );
//         assert_eq!(stats.durations.iter().filter(|d| d.is_some()).count(), 0);
//         Ok(())
//     }

//     static INIT: Once = Once::new();

//     #[cfg(test)]
//     #[ctor::ctor]
//     fn init() {
//         INIT.call_once(|| {
//             env_logger::builder()
//                 .is_test(true)
//                 .filter_level(LevelFilter::Info)
//                 .filter_module("v2ray_monitor", LevelFilter::Trace)
//                 .init();
//         });
//     }

// }
//     #[cfg(test)]
//     mod v2ray_tests {
//         use super::*;

//         #[tokio::test]
//         async fn tcp_ping() -> Result<()> {
//             let v2 = V2ray::new(Default::default());
//             let pp = Default::default();
//             let nodes = vec![get_node(), get_node(), get_node()];
//             let ps = v2.tcp_ping_nodes(nodes, &pp).await;
//             log::debug!("{:?}", ps);
//             Ok(())
//         }
//     }

use std::{sync::Arc, time::Duration};

use super::{
    filter::*,
    v2ray_task_config::{PingProperty, TcpPingTaskProperty},
    RetryService,
};
use crate::{
    tcp_ping::{self, TcpPingStatistic},
    v2ray::{node::Node, V2rayService},
};
use anyhow::{anyhow, Result};

use parking_lot::Mutex;
use tokio::{
    sync::mpsc::{Receiver, Sender},
    time,
};

pub struct TcpPingTask {
    // nodes:
    prop: TcpPingTaskProperty,
    nodes: Arc<Mutex<Vec<Node>>>,
    v2: Arc<dyn V2rayService>,
    pre_filter: Option<Arc<NameRegexFilter>>,
}

impl TcpPingTask {
    pub fn new(prop: TcpPingTaskProperty, v2: Arc<dyn V2rayService>) -> Self {
        Self {
            v2,
            nodes: Arc::new(Mutex::new(vec![])),
            // sender,
            pre_filter: prop
                .filter
                .name_regex
                .as_ref()
                .map(|s| Arc::new(NameRegexFilter::new(s))),
            prop,
        }
    }

    async fn update_nodes(&self, mut rx: Receiver<Vec<Node>>) -> Result<()> {
        log::debug!("Waiting to receive nodes data");
        let recv_nodes = rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("receive nodes error"))?;
        let pre_filter = self.pre_filter.clone();
        let nodes = self.nodes.clone();
        *nodes.lock() = if let Some(f) = pre_filter.as_ref() {
            f.filter(recv_nodes)
        } else {
            recv_nodes
        };
        tokio::spawn(async move {
            while let Some(recv_nodes) = rx.recv().await {
                log::debug!("processing received nodes: {}", recv_nodes.len());
                *nodes.lock() = if let Some(f) = pre_filter.as_ref() {
                    f.filter(recv_nodes)
                } else {
                    recv_nodes
                };
            }
        });
        Ok(())
    }

    pub async fn run(
        &self,
        rx: Receiver<Vec<Node>>,
        tx: Sender<Vec<(Node, TcpPingStatistic)>>,
    ) -> Result<()> {
        self.update_nodes(rx).await?;
        let retry_srv = RetryService::new(self.prop.retry.clone());
        let mut interval = time::interval(self.prop.update_interval);
        loop {
            let nodes = self.nodes.clone().lock().clone();
            let ping_prop = self.prop.ping.clone();
            let v2 = self.v2.clone();
            let tx = tx.clone();
            interval.tick().await;
            match retry_srv
                .retry_on(
                    move || do_ping(v2.clone(), nodes.clone(), ping_prop.clone(), tx.clone()),
                    false,
                )
                .await
            {
                Ok(a) => {
                    log::debug!("tcp ping task successfully duration: {:?}", a.1);
                }
                Err(e) => {
                    log::error!("tcp ping retry all error: {}", e);
                }
            }
        }
    }
}
async fn do_ping(
    v2: Arc<dyn V2rayService>,
    nodes: Vec<Node>,
    ping_prop: PingProperty,
    tx: Sender<Vec<(Node, TcpPingStatistic)>>,
) -> Result<()> {
    let old_len = nodes.len();
    let (node_stats, err_nodes) = tcp_ping::ping_batch(v2, nodes, &ping_prop).await?;
    if node_stats.is_empty() {
        log::error!(
            "no any available nodes. error nodes: {:?}, old_len: {}",
            err_nodes.as_ref().map(|n| n.len()),
            old_len,
        );
        return Err(anyhow!("no any available nodes after tcp ping"));
    }
    if let Some(err_nodes) = err_nodes {
        log::info!(
            "{} error nodes found after ping: {:?}",
            err_nodes.len(),
            err_nodes
                .iter()
                .map(|n| n.remark.as_ref())
                .collect::<Vec<_>>()
        );
    }
    log::debug!("sending node stats: {}", node_stats.len());
    tx.send_timeout(node_stats, Duration::from_secs(10)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::channel;

    use crate::{
        task::{find_v2ray_bin_path, v2ray_task_config::LocalV2rayProperty},
        v2ray::{node, LocalV2rayService},
    };

    use super::*;

    #[tokio::test]
    async fn basic() -> Result<()> {
        let prop = TcpPingTaskProperty::default();
        let (tx, mut rx) = channel::<Vec<(Node, TcpPingStatistic)>>(1);
        do_ping(local_v2(), get_nodes().await?, prop.ping, tx).await?;
        let nodes = rx.recv().await;
        assert!(nodes.is_some());
        assert!(!nodes.unwrap().is_empty());
        Ok(())
    }

    fn local_v2() -> Arc<dyn V2rayService> {
        Arc::new(LocalV2rayService::new(get_local_prop().unwrap()))
    }

    fn get_local_prop() -> Result<LocalV2rayProperty> {
        Ok(LocalV2rayProperty {
            bin_path: find_v2ray_bin_path()?,
            config_path: Some("tests/data/local-v2-config.json".to_string()),
        })
    }

    async fn get_nodes() -> Result<Vec<Node>> {
        let mut nodes =
            node::load_subscription_nodes_from_file("tests/data/v2ray-subscription.txt").await?;
        nodes.retain(|node| node.remark.as_ref().unwrap().contains("专线"));
        Ok(nodes)
    }
}

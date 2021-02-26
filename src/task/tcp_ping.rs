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

pub struct TcpPingTask<V: V2rayService> {
    // nodes:
    prop: TcpPingTaskProperty,
    nodes: Arc<Mutex<Vec<Node>>>,
    v2: V,
    pre_filter: Option<Arc<NameRegexFilter>>,
}

impl<V: V2rayService> TcpPingTask<V> {
    pub fn new(prop: TcpPingTaskProperty, v2: V) -> Self {
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

        let task = {
            let v2 = self.v2.clone();
            let nodes = self.nodes.clone().lock().clone();
            let ping_prop = self.prop.ping.clone();
            // let tx = tx.clone();
            Arc::new(move || dotask(v2.clone(), nodes.clone(), ping_prop.clone(), tx.clone()))
        };
        let mut interval = time::interval(self.prop.update_interval);
        loop {
            interval.tick().await;
            match retry_srv.retry_on(task.clone().as_ref(), false).await {
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
async fn dotask<V: V2rayService>(
    v2: V,
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
    use crate::{
        task::{find_v2ray_bin_path, v2ray_task_config::LocalV2rayProperty},
        v2ray::LocalV2ray,
    };

    use super::*;

    #[tokio::test]
    async fn basic() -> Result<()> {
        let _v2 = LocalV2ray::new(get_local_prop()?);
        let _prop = get_tcp_ping_task_prop()?;
        // let a = TcpPingTask::new(prop, v2, None);
        // a.run().await?;
        Ok(())
    }

    fn get_local_prop() -> Result<LocalV2rayProperty> {
        Ok(LocalV2rayProperty {
            bin_path: find_v2ray_bin_path()?,
            config_path: Some("tests/data/local-v2-config.json".to_string()),
        })
    }

    fn get_tcp_ping_task_prop() -> Result<TcpPingTaskProperty> {
        todo!()
    }
}

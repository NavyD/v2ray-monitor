use std::sync::Arc;

use super::{
    filter::*,
    v2ray_task_config::{PingProperty, TcpPingTaskProperty},
    RetryService, TaskRunnable,
};
use crate::{
    tcp_ping::{self, TcpPingStatistic},
    v2ray::{
        node::{load_subscription_nodes_from_file, Node},
        V2rayService,
    },
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::mpsc::{Receiver, Sender, channel};

pub struct TcpPingTask<V: V2rayService> {
    // nodes:
    prop: TcpPingTaskProperty,
    nodes: Arc<Mutex<Vec<Node>>>,
    v2: V,
    sender: Sender<Vec<(Node, TcpPingStatistic)>>,
    rx: Arc<Mutex<Receiver<Vec<Node>>>>,
    pre_filter: Box<dyn Filter<Vec<Node>, Vec<Node>>>,
}

impl<V: V2rayService> TcpPingTask<V> {
    pub fn new(
        prop: TcpPingTaskProperty,
        v2: V,
        sender: Sender<Vec<(Node, TcpPingStatistic)>>,
        rx: Receiver<Vec<Node>>,
    ) -> Self {
        Self {
            v2,
            nodes: Arc::new(Mutex::new(vec![])),
            sender,
            pre_filter: Box::new(NameRegexFilter::new(&[prop
                .filter
                .name_regex
                .clone()
                .unwrap()])),
            prop,
            rx: todo!(),
        }
    }
}

#[async_trait]
impl<V: V2rayService> TaskRunnable for TcpPingTask<V> {
    async fn run(&self) -> anyhow::Result<()> {
        // let rx = self.rx.clone();
        let (tx, rx) = channel(1);
        tokio::spawn(async move {
            tx.send(1).await.expect("");
        });

        // loop {
        //     let ping_prop = self.prop.ping.clone();
        //     let v2 = self.v2.clone();

        //     let retry_srv = RetryService::new(self.prop.retry_failed.clone(), move || {
        //         task(
        //             v2.clone(),
        //             nodes.clone(),
        //             ping_prop.clone(),
        //             self.sender.clone(),
        //         )
        //     });
        // }
        Ok(())
    }
}

async fn task<V: V2rayService>(
    v2: V,
    nodes: Vec<Node>,
    ping_prop: PingProperty,
    sender: Sender<Vec<(Node, TcpPingStatistic)>>,
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
        )
    }
    // if let Err(nodes) = sender.send(node_stats) {
    //     return Err(anyhow!("send error nodes: {}", nodes.len()));
    // }
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
        let v2 = LocalV2ray::new(get_local_prop()?);
        let prop = get_tcp_ping_task_prop()?;
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

use std::sync::Arc;

use crate::v2ray::node::*;

use super::{v2ray_task_config::SubscriptionTaskProperty, RetryService};
use anyhow::Result;

use tokio::{fs::File, io::AsyncReadExt, sync::mpsc::Sender, time::sleep};
pub struct SubscriptionTask {
    // nodes:
    prop: SubscriptionTaskProperty,
}

impl SubscriptionTask {
    pub fn new(prop: SubscriptionTaskProperty) -> Self {
        Self { prop }
    }
    /// 根据订阅文件自动更新并加载到内存中。
    ///
    pub async fn run(&self, tx: Sender<Vec<Node>>) -> Result<()> {
        let subscpt = self.prop.clone();
        let retry_srv = RetryService::new(subscpt.retry_failed.clone());

        if let Err(e) = load_from_local(&subscpt, &tx).await {
            log::warn!("Failed to load the subscription node locally: {}", e);
        }
        let task = {
            let url = subscpt.url.clone();
            let path = subscpt.path.clone();
            Arc::new(move || update_subscription(url.clone(), path.clone(), tx.clone()))
        };

        loop {
            log::debug!("Loading nodes from network url: {}", subscpt.url);
            match retry_srv.retry_on(task.as_ref(), false).await {
                Ok((retries, duration)) => {
                    log::info!(
                        "update subscription task successful on retries: {}, duration: {:?}",
                        retries,
                        duration
                    );
                }
                Err(e) => {
                    log::error!("update subscription task failed: {}", e);
                }
            };
            log::debug!(
                "update subscription task sleeping with interval: {:?}",
                subscpt.update_interval
            );
            sleep(subscpt.update_interval).await;
        }
    }
}

async fn load_from_local(subscpt: &SubscriptionTaskProperty, tx: &Sender<Vec<Node>>) -> Result<()> {
    // 根据文件修改日期 找到第一次等待时间
    let first_interval = {
        log::debug!("Opening subscription file from path: {}", subscpt.path);
        let mut file = File::open(&subscpt.path).await?;
        // 读取节点
        let mut contents = String::new();
        file.read_to_string(&mut contents).await?;
        let nodes = parse_subx_nodes(contents)?;
        log::trace!("sending parsed nodes: {}", nodes.len());
        tx.send(nodes).await?;

        let md_interval = file.metadata().await?.modified()?.elapsed()?;
        log::debug!(
            "{} modified elapsed duration: {:?}",
            subscpt.path,
            md_interval
        );
        subscpt.update_interval.checked_sub(md_interval)
    };

    if let Some(interval) = first_interval {
        log::info!(
            "waiting update duration: {:?} from last file {} modified",
            interval,
            subscpt.path,
        );
        sleep(interval).await;
    }
    Ok(())
}

async fn update_subscription(url: String, path: String, tx: Sender<Vec<Node>>) -> Result<()> {
    let contents = reqwest::get(&url).await?.bytes().await?;
    log::trace!(
        "writing to {} for subscription contents len: {}",
        path,
        contents.len()
    );

    let nodes = parse_subx_nodes(&contents)?;
    log::trace!("sending parsed nodes: {}", nodes.len());
    tx.send(nodes).await?;

    tokio::fs::write(path, &contents).await?;
    log::trace!("parsing subscription data");
    Ok(())
}

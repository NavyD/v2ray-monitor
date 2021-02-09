use std::time::Duration;

use task::v2ray_tasks::V2rayTask;
use tokio::time::sleep;

mod config;
mod node;
mod task;
mod v2ray;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .filter_module("reqwest", log::LevelFilter::Info)
        .init();
    let mut v2 = V2rayTask::with_default();
    // v2.proxy.node_name_regex = Some("→香港02".to_owned());
    v2.run().await?;
    loop {
        sleep(Duration::from_secs(30)).await;
    }
}

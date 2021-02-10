use std::time::Duration;

use task::v2ray_task_config::*;
use task::v2ray_tasks::V2rayTask;
use tokio::{
    fs::{read_to_string, File},
    time::sleep,
};
use v2ray::V2ray;

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
    let contents = read_to_string("config.yaml").await?;
    let config: V2rayTaskProperty = serde_yaml::from_str(&contents)?;
    let v2 = V2rayTask::new(config);
    // let v2 = V2rayTask::with_default();
    // v2..node_name_regex = Some("→香港02".to_owned());
    v2.run().await?;
    // let v = V2ray::new(Default::default());
    // v.restart_load_balance(&[]).await?;
    loop {
        sleep(Duration::from_secs(30)).await;
    }
}

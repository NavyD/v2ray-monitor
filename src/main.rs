use std::time::Duration;

use env_logger::Env;
use task::v2ray_task_config::*;
use task::v2ray_tasks::V2rayTask;
use tokio::{fs::read_to_string, time::sleep};

mod config;
mod node;
mod task;
mod v2ray;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("debug"))
        .filter_module("reqwest", log::LevelFilter::Info)
        .init();
    let contents = read_to_string("config.yaml").await?;
    let config: V2rayTaskProperty = serde_yaml::from_str(&contents)?;
    let v2 = V2rayTask::new(config);
    v2.run().await?;
    loop {
        sleep(Duration::from_secs(60 * 60 * 24)).await;
    }
}

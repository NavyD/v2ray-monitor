use task::V2rayTask;

mod config;
mod node;
mod task;
mod v2ray;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .init();
    V2rayTask::with_default().run().await
}

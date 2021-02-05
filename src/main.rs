use tokio::sync::mpsc::channel;
// use surf::{Client, Request, Response, http::headers::FORWARDED, middleware::{Middleware, Next}};
use v2ray::{PingProperty, V2ray, V2rayProperty};

mod config;
mod node;
mod v2ray;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .init();
    run().await
}

async fn run() -> anyhow::Result<()> {
    let path = "/home/navyd/Workspaces/projects/router-tasks/v2ray-subscription.txt";
    let mut nodes = node::load_subscription_nodes_from_file(path).await?;
    let pp = PingProperty::default();
    let vp = V2rayProperty::default();
    let (tx, mut rx) = channel(1);
    let v2ray = V2ray::new(pp, vp);
    let count = nodes.len();

    for _ in 0..count {
        let node = nodes.pop().unwrap();
        let v2ray = v2ray.clone();
        let tx = tx.clone();
        // let stats = v2ray.tcp_ping(&node).await.unwrap();
        // log::info!("{} stats: {:?}", node.remark.unwrap(), stats);
        // tx.send(stats).await.unwrap();

        tokio::spawn(async move {
            let stats = v2ray.tcp_ping(&node).await;
            log::info!("{} stats: {:?}", node.remark.unwrap(), stats);
            tx.send(stats).await.unwrap();
        });
    }
    for i in 0..count {
        let stats = rx.recv().await.unwrap();
        log::error!("{} received: {:?}", i, stats);
    }
    Ok(())
}

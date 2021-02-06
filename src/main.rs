use std::time::Duration;

use anyhow::Error;
use tokio::{net::{TcpListener, TcpStream}, sync::mpsc::channel};
// use surf::{Client, Request, Response, http::headers::FORWARDED, middleware::{Middleware, Next}};
use v2ray::{PingProperty, V2rayProperty};

mod config;
mod node;
mod v2ray;
mod task;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .init();
    // run().await?;
    Ok(())
}

// async fn run() -> anyhow::Result<()> {
//     let path = "/home/navyd/Workspaces/projects/router-tasks/v2ray-subscription.txt";
//     let nodes = node::load_subscription_nodes_from_file(path).await?;
//     let pp = PingProperty::default();
//     let vp = V2rayProperty {
//         concurr_num: nodes.len(),
//         ..V2rayProperty::default()
//     };

//     let v2ray = V2ray::new(pp, vp);

//     let ps = v2ray.tcp_ping_nodes(nodes).await?;
//     // log::info!("all ps: {:?}", ps);
//     Ok(())
// }

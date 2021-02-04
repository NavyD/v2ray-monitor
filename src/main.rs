use v2ray::{PingProperty, V2ray, V2rayProperty};

mod config;
mod node;
mod v2ray;

fn main() {
    env_logger::builder()
        .filter(Some("reqwest"), log::LevelFilter::Info)
        .filter_level(log::LevelFilter::Info)
        .init();
    smol::block_on(async {
        run().await.unwrap();
    });
}

async fn run() -> anyhow::Result<()> {
    let path = "/home/navyd/Workspaces/projects/router-tasks/v2ray-subscription.txt";
    let mut nodes = node::load_subscription_nodes_from_file(path).await?;
    let pp = PingProperty::default();
    let vp = V2rayProperty {
        bin_path: "/home/navyd/Downloads/v2ray/v2ray".to_owned(),
        config_path: "/home/navyd/Downloads/v2ray/v2-config.json".to_owned(),
    };
    let (tx, rx) = smol::channel::bounded(1);
    let v2ray = V2ray::new(pp, vp);
    let count = nodes.len() / 2;

    for _ in 0..count {
        let node = nodes.pop().unwrap();
        let v2ray = v2ray.clone();
        let tx = tx.clone();
        // let stats = v2ray.tcp_ping(&node).await.unwrap();
        // log::info!("{} stats: {:?}", node.remark.unwrap(), stats);
        // tx.send(stats).await.unwrap();

        smol::spawn(async move {
            let stats = v2ray.tcp_ping(&node).await.unwrap();
            log::info!("{} stats: {:?}", node.remark.unwrap(), stats);
            tx.send(stats).await.unwrap();
        })
        .detach();
    }
    for _ in 0..count {
        let stats = rx.recv().await.unwrap();
        log::error!("received: {:?}", stats);
    }
    Ok(())
}

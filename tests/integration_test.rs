use std::{sync::Once, time::Duration};

use log::LevelFilter;
use tokio::{fs::read_to_string, sync::mpsc::channel, time::sleep};
use v2ray_monitor::{
    task::{
        subscription::SubscriptionTask,
        switch::SwitchTask,
        tcp_ping::TcpPingTask,
        v2ray_task_config::{RetryIntevalAlgorithm, V2rayTaskProperty},
    },
    v2ray::LocalV2ray,
};

#[tokio::test]
async fn tcp_ping_to_switch() -> anyhow::Result<()> {
    let config = read_to_string("tests/data/config.yaml").await?;
    let mut config = serde_yaml::from_str::<V2rayTaskProperty>(&config)?;
    config.tcp_ping.ping.concurr_num = 24;
    config.tcp_ping.filter.name_regex = Some("东01→台湾02".to_string());
    config.switch.retry.count = 3;
    // config.switch.filter.name_regex = Some("粤".to_string());
    config.switch.retry.interval_algo = RetryIntevalAlgorithm::SwitchBeb {
        min: Duration::from_millis(100),
        max: Duration::from_millis(5000),
        switch_limit: 3,
    };
    config.switch.local.config_path = None;
    let (nodes_tx, nodes_rx) = channel(1);

    let subscpt = SubscriptionTask::new(config.subscpt.clone());
    tokio::spawn(async move {
        subscpt.run(nodes_tx).await.unwrap();
    });

    let v2 = LocalV2ray::new(config.switch.local.clone());
    let ping_task = TcpPingTask::new(config.tcp_ping.clone(), v2.clone());
    let (stats_tx, stats_rx) = channel(1);

    tokio::spawn(async move {
        ping_task.run(nodes_rx, stats_tx).await.unwrap();
    });

    let switch_task = SwitchTask::new(config.switch.clone(), v2);
    tokio::spawn(async move {
        switch_task.run(stats_rx).await.unwrap();
    });
    loop {
        sleep(Duration::from_secs(2000)).await;
    }
}

static INIT: Once = Once::new();

#[cfg(test)]
#[ctor::ctor]
fn init() {
    INIT.call_once(|| {
        env_logger::builder()
            .is_test(true)
            .filter_level(LevelFilter::Info)
            // .filter_module("reqwest", LevelFilter::Debug)
            .filter_module("v2ray_monitor", LevelFilter::Trace)
            .init();
        // TEST_DIR.set(Path::new("tests/data").to_path_buf());
    });
}

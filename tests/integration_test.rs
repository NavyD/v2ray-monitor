use std::{sync::Once, time::Duration};

use log::LevelFilter;
use tokio::{fs::read_to_string, time::sleep};
use v2ray_monitor::{task::v2ray_task_config::V2rayTaskProperty, V2rayTaskManager};

#[tokio::test]
#[ignore]
async fn tcp_ping_to_switch() -> anyhow::Result<()> {
    let config = read_to_string("tests/data/config.yaml").await?;
    let config = serde_yaml::from_str::<V2rayTaskProperty>(&config)?;
    let mut task = V2rayTaskManager::new(config);
    task.run().await;
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
            .filter_module("v2ray_monitor", LevelFilter::Debug)
            .init();
        // TEST_DIR.set(Path::new("tests/data").to_path_buf());
    });
}

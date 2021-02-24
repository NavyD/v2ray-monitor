use std::path::Path;

use anyhow::Result;
use task::{
    jinkela_checkin::JinkelaCheckinTask,
    subscription::SubscriptionTask,
    switch::SwitchTask,
    tcp_ping::TcpPingTask,
    v2ray_task_config::{V2rayTaskProperty, V2rayType},
};
use tokio::{fs::read_to_string, sync::mpsc::channel};
use v2ray::{LocalV2ray, SshV2ray};
mod client;
pub mod task;
mod tcp_ping;
pub mod v2ray;

pub struct V2rayTaskManager {
    prop: V2rayTaskProperty,
    local_v2: LocalV2ray,
    ssh_v2: Option<SshV2ray>,
}

impl V2rayTaskManager {
    pub fn new(prop: V2rayTaskProperty) -> Self {
        Self {
            local_v2: LocalV2ray::new(prop.v2ray.local.clone()),
            ssh_v2: prop.v2ray.ssh.clone().map(SshV2ray::new),
            prop,
        }
    }

    pub async fn from_path<T: AsRef<Path>>(path: T) -> Result<Self> {
        let config = read_to_string(path).await?;
        let config = serde_yaml::from_str::<V2rayTaskProperty>(&config)?;
        Ok(Self::new(config))
    }

    pub async fn run(&mut self) {
        // start subscription task
        let (nodes_tx, nodes_rx) = channel(1);
        let subscpt = self.prop.subscpt.clone();
        tokio::spawn(async move {
            SubscriptionTask::new(subscpt).run(nodes_tx).await.unwrap();
        });

        // start ping task
        let (stats_tx, stats_rx) = channel(1);
        let local_v2 = self.local_v2.clone();
        let ssh_v2 = self.ssh_v2.clone();
        let ping_prop = self.prop.tcp_ping.clone();
        tokio::spawn(async move {
            match &ping_prop.v2_type {
                V2rayType::Local => {
                    TcpPingTask::new(ping_prop, local_v2.clone())
                        .run(nodes_rx, stats_tx)
                        .await
                        .unwrap();
                }
                V2rayType::Ssh => {
                    TcpPingTask::new(ping_prop, ssh_v2.clone().expect("not found v2ray ssh"))
                        .run(nodes_rx, stats_tx)
                        .await
                        .unwrap();
                }
            };
        });

        // start switch task
        let local_v2 = self.local_v2.clone();
        let ssh_v2 = self.ssh_v2.clone();
        let switch_prop = self.prop.switch.clone();
        match &switch_prop.v2_type {
            V2rayType::Local => {
                SwitchTask::new(switch_prop, local_v2.clone())
                    .run(stats_rx)
                    .await
                    .unwrap();
            }
            V2rayType::Ssh => {
                SwitchTask::new(switch_prop, ssh_v2.clone().expect("not found v2ray ssh"))
                    .run(stats_rx)
                    .await
                    .unwrap();
            }
        };

        if let Some(prop) = self.prop.jinkela.take() {
            let jinkela = JinkelaCheckinTask::new(prop);
            tokio::spawn(async move { jinkela.run().await.unwrap() });
        }
    }
}

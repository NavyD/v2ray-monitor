use std::{net::IpAddr, path::Path, sync::Arc};

use anyhow::Result;
use task::{
    dns_flush,
    jinkela_checkin::JinkelaCheckinTask,
    subscription::SubscriptionTask,
    switch::SwitchTask,
    tcp_ping::TcpPingTask,
    v2ray_task_config::{V2rayTaskProperty, V2rayType},
};
use tokio::{fs::read_to_string, sync::mpsc::channel};
use v2ray::{LocalV2rayService, SshV2rayService, V2rayService};
mod client;
pub mod task;
mod tcp_ping;
pub mod v2ray;

pub struct V2rayTaskManager {
    prop: V2rayTaskProperty,
    local_v2: Arc<dyn V2rayService>,
    ssh_v2: Arc<dyn V2rayService>,
}

impl V2rayTaskManager {
    pub fn new(prop: V2rayTaskProperty) -> Self {
        Self {
            local_v2: Arc::new(LocalV2rayService::new(prop.v2ray.local.clone())),
            ssh_v2: Arc::new(SshV2rayService::new(prop.v2ray.ssh.clone().unwrap())),
            prop,
        }
    }

    pub async fn from_path<T: AsRef<Path>>(path: T) -> Result<Self> {
        let config = read_to_string(path).await?;
        let config = serde_yaml::from_str::<V2rayTaskProperty>(&config)?;
        Ok(Self::new(config))
    }

    fn get_v2(&self, ty: &V2rayType) -> Arc<dyn V2rayService> {
        match ty {
            V2rayType::Local => self.local_v2.clone(),
            V2rayType::Ssh => self.ssh_v2.clone(),
        }
    }

    pub async fn run(&mut self) {
        // start subscription task
        let (nodes_tx, nodes_rx) = channel(1);
        let subx = self.prop.subx.clone();
        tokio::spawn(async move {
            SubscriptionTask::new(subx).run(nodes_tx).await.unwrap();
        });

        // start ping task
        let (stats_tx, stats_rx) = channel(1);
        let ping_prop = self.prop.tcp_ping.clone();
        let v2 = self.get_v2(&ping_prop.v2_type);
        tokio::spawn(async move {
            TcpPingTask::new(ping_prop, v2)
                .run(nodes_rx, stats_tx)
                .await
                .unwrap();
        });

        let (ips_tx, ips_rx) = channel::<Vec<IpAddr>>(1);
        let dns_task = dns_flush::HostDnsFlushTask::new(self.prop.dns.clone());
        tokio::spawn(async move {
            dns_task.run(ips_tx).await.unwrap();
        });

        // start switch task
        let switch_prop = self.prop.switch.clone();
        let v2 = self.get_v2(&switch_prop.v2_type);
        tokio::spawn(async move {
            SwitchTask::new(switch_prop, v2)
                .run(stats_rx, ips_rx)
                .await
                .unwrap();
        });

        if let Some(prop) = self.prop.jinkela.take() {
            let jinkela = JinkelaCheckinTask::new(prop);
            tokio::spawn(async move { jinkela.run().await.unwrap() });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Once;

    use log::LevelFilter;
    static INIT: Once = Once::new();

    #[cfg(test)]
    #[ctor::ctor]
    fn init() {
        INIT.call_once(|| {
            let crate_name = module_path!()
                .split("::")
                .collect::<Vec<_>>()
                .first()
                .cloned()
                .expect("get module_path error");
            env_logger::builder()
                .is_test(true)
                .filter_level(LevelFilter::Info)
                .filter_module(crate_name, LevelFilter::Trace)
                // .filter_module(&(crate_name.to_string() + "::tcp_ping"), LevelFilter::Debug)
                .init();
            // TEST_DIR.set(Path::new("tests/data").to_path_buf());
        });
    }
}

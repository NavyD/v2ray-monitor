use std::time::Duration;

use env_logger::Env;

use tokio::{fs::read_to_string, sync::mpsc::channel, time::sleep};
use v2ray_monitor::{
    self,
    task::{switch::SwitchTask, tcp_ping::TcpPingTask, v2ray_task_config::V2rayTaskProperty},
    v2ray::{node::load_subscription_nodes_from_file, LocalV2ray},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let crate_name = module_path!()
        .split("::")
        .collect::<Vec<_>>()
        .first()
        .cloned()
        .expect("get module_path error");
    let opt = Opt::from_args();
    env_logger::Builder::from_env(Env::new().default_filter_or("info"))
        .filter_module(crate_name, opt.log_level)
        .init();

    log::debug!("config path: {}", opt.config.to_str().unwrap());

    loop {
        sleep(Duration::from_secs(60 * 60 * 24)).await;
    }
}

use std::path::PathBuf;
use structopt::StructOpt;

#[derive(StructOpt, Debug)]
#[structopt()]
struct Opt {
    #[structopt(long, parse(from_os_str), default_value = "config.yaml")]
    config: PathBuf,

    #[structopt(long, parse(try_from_str), default_value = "trace")]
    log_level: log::LevelFilter,
}

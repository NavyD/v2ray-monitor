use std::time::Duration;

use env_logger::Env;
use task::v2ray_task_config::*;
use tokio::{fs::read_to_string, time::sleep};

mod task;
mod tcp_ping;
mod v2ray;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let crate_name = module_path!()
        .split("::")
        .collect::<Vec<_>>()
        .first()
        .cloned()
        .expect("get module_path error");
    let opt = Opt::from_args();
    env_logger::Builder::from_env(Env::new().default_filter_or("warn"))
        .filter_module(crate_name, opt.log_level)
        .init();

    log::debug!("config path: {}", opt.config.to_str().unwrap());
    // let config: V2rayTaskProperty = serde_yaml::from_str(&read_to_string(opt.config).await?)?;
    // let v2 = V2rayTask::new(config);
    // v2.run().await?;
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

    #[structopt(long, parse(try_from_str), default_value = "warn")]
    log_level: log::LevelFilter,
}

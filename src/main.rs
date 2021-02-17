use std::time::Duration;

use env_logger::Env;

use tokio::time::sleep;

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

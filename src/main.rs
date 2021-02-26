use std::time::Duration;

use chrono::{DateTime, Local};

use env_logger::Env;

use tokio::time::sleep;
use v2ray_monitor::V2rayTaskManager;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opt = Opt::from_args();
    init_log(&opt);
    log::debug!(
        "loading task config from path: {}",
        opt.config.to_str().unwrap()
    );

    let mut task = V2rayTaskManager::from_path(opt.config).await?;
    task.run().await;

    loop {
        sleep(Duration::from_secs(60 * 60 * 24)).await;
    }
}

fn init_log(opt: &Opt) {
    let crate_name = module_path!()
        .split("::")
        .collect::<Vec<_>>()
        .first()
        .cloned()
        .expect("get module_path error");
    env_logger::Builder::from_env(Env::new().default_filter_or("warn"))
        .filter_module(crate_name, opt.log_level)
        .format(|buf, record| {
            use std::io::Write;
            let ts = buf
                .timestamp()
                .to_string()
                .parse::<DateTime<Local>>()
                .unwrap()
                .format("%Y-%m-%d %H:%M:%S");
            let style = buf.default_level_style(record.level());
            // [2021-02-19T03:31:42Z WARN  v2ray_monitor::tcp_ping]
            writeln!(
                buf,
                "[{} {} {}] {}",
                ts,
                style.value(record.level()),
                record.module_path_static().unwrap(),
                record.args()
            )
        })
        .init();
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

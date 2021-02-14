pub mod config;
pub mod node;

use crate::task::v2ray_task_config::*;
use async_trait::async_trait;
use futures::executor::block_on;
use once_cell::sync::OnceCell;
use std::{
    borrow::{Borrow, BorrowMut},
    cmp::Ordering,
    env::{split_paths, var_os},
    fs,
    ops::{Deref, DerefMut},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};

use reqwest::Proxy;
use tokio::{fs::read_to_string, io::*, net::TcpListener, process::{Child, Command}, runtime::Runtime, sync::{mpsc::channel, Mutex, Semaphore}};

use self::node::Node;

#[async_trait]
pub trait ConfigurableV2ray {
    async fn get_config(&self) -> Result<&str>;

    async fn apply_config(&self, nodes: &[&Node]) -> Result<String>;
}

#[async_trait]
pub trait V2rayService: Send + Sync + Clone + ConfigurableV2ray {
    async fn start(&self, config: &str) -> Result<Child>;

    async fn start_in_background(&self, config: &str) -> Result<()>;

    async fn stop(&self) -> Result<()>;

    async fn stop_all(&self) -> Result<()>;

    async fn get_available_port(&self) -> Result<u16>;

    async fn restart_in_background(&self, config: &str) -> Result<()> {
        self.stop_all().await?;
        self.start_in_background(config).await
    }
}

#[derive(Clone)]
pub struct SshV2ray {
    prop: Arc<SshV2rayProperty>,
    config: OnceCell<String>,
}

impl SshV2ray {
    pub fn new(prop: SshV2rayProperty) -> Self {
        Self {
            prop: Arc::new(prop),
            config: OnceCell::new(),
        }
    }

    async fn exe_ssh(&self, sh_cmd: &str) -> Result<()> {
        let out = tokio::process::Command::new("ssh")
            .arg(&format!("{}@{}", self.prop.username, self.prop.host))
            .arg(&sh_cmd)
            .stdout(Stdio::piped())
            .output()
            .await?;
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr);
            log::error!(
                "execute ssh command failed from `ssh {}@{} '{}'` error: {}",
                self.prop.username,
                self.prop.host,
                sh_cmd,
                msg
            );
            return Err(anyhow!("execute ssh error: {}", msg));
        } else {
            log::trace!(
                "ssh success output: {}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
        Ok(())
    }
}

#[async_trait]
impl ConfigurableV2ray for SshV2ray {
    /// 使用scp username@host:path读取配置到内存中
    ///
    /// # Errors
    ///
    /// 如果命令执行失败
    async fn get_config(&self) -> Result<&str> {
        async fn scp_config(prop: &SshV2rayProperty) -> Result<String> {
            let sh_cmd = format!(
                "scp {}@{}:{} /dev/stdout",
                prop.username, prop.host, prop.config_path
            );
            log::trace!("loading config from ssh command: {}", sh_cmd);
            let args = sh_cmd.split(' ').collect::<Vec<_>>();
            let out = Command::new(args[0]).args(&args[1..]).output().await?;
            if !out.status.success() {
                let msg = String::from_utf8_lossy(&out.stderr);
                log::error!("get config from ssh {} error: {}", sh_cmd, msg);
                return Err(anyhow!("get config ssh error: {}", msg));
            }
            let content = String::from_utf8(out.stdout)?;
            log::trace!("get ssh config output: {}", content);
            Ok(content)
        }
        let config = self.config.get_or_try_init(|| {
            let prop = self.prop.clone();
            let rt  = Runtime::new().unwrap();
            rt.block_on(async { scp_config(&prop).await })
        })?;
        Ok(config)
    }

    async fn apply_config(&self, nodes: &[&Node]) -> Result<String> {
        let contents = self.get_config().await?;
        config::apply_config(&contents, nodes, None)
    }
}

#[async_trait]
impl V2rayService for SshV2ray {
    async fn start(&self, config: &str) -> Result<Child> {
        let sh_cmd = format!(
            "echo '{}' | nohup v2ray -config stdin: &> /dev/null &",
            config
        );
        log::trace!("starting with ssh command: {}", sh_cmd);
        let child = Command::new("ssh")
            .arg(&format!("{}@{}", self.prop.username, self.prop.host))
            .arg(&sh_cmd)
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        Ok(child)
    }

    async fn start_in_background(&self, config: &str) -> Result<()> {
        let sh_cmd = format!(
            "echo '{}' | nohup v2ray -config stdin: &> /dev/null &",
            config
        );
        log::trace!("starting in background with ssh command: {}", sh_cmd);
        self.exe_ssh(&sh_cmd).await
    }

    async fn stop(&self) -> Result<()> {
        // 可以在background时读取进程id保存，然后ssh kill
        todo!()
    }

    async fn stop_all(&self) -> Result<()> {
        // clean ssr-monitor and killall
        let sh_cmd = {
            let kill_ssr_monitor = "ps -ef | grep ssr-monitor | grep -v grep | awk '{print $1}' | xargs kill -9 && echo 'killed ssr-monitor on busbox'";
            let kill_v2ray = "killall -9 v2ray && echo 'killed v2ray'";
            format!("{};{};", kill_ssr_monitor, kill_v2ray)
        };
        log::trace!("stopping v2ray with ssh command: {}", sh_cmd);
        self.exe_ssh(&sh_cmd).await
    }

    async fn restart_in_background(&self, config: &str) -> Result<()> {
        // 0. clean v2ray env on ssh host
        let sh_cmd = {
            let kill_ssr_monitor = "ps -ef | grep ssr-monitor | grep -v grep | awk '{print $1}' | xargs kill -9 && echo 'killed ssr-monitor on busbox'";
            let kill_v2ray = "killall -9 v2ray && echo 'killed v2ray'";
            format!(
                "{};{};echo '{}' | nohup v2ray -config stdin: &> /dev/null &",
                kill_ssr_monitor, kill_v2ray, config
            )
        };
        log::trace!("restarting v2ray with ssh command: {}", sh_cmd);
        // 1. start v2ray
        let out = tokio::process::Command::new("ssh")
            .arg(&format!("{}@{}", self.prop.username, self.prop.host))
            .arg(&sh_cmd)
            .stdout(Stdio::piped())
            .output()
            .await?;
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr);
            log::error!(
                "get config from `ssh {}@{} '{}'` error: {}",
                self.prop.username,
                self.prop.host,
                sh_cmd,
                msg
            );
            return Err(anyhow!("get config ssh error: {}", msg));
        } else {
            log::trace!(
                "get ssh config output: {}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
        Ok(())
    }

    async fn get_available_port(&self) -> Result<u16> {
        let listen = TcpListener::bind(format!("{}:0", self.prop.host)).await?;
        let addr = listen.local_addr()?;
        Ok(addr.port())
    }
}

#[derive(Clone)]
pub struct LocalV2ray {
    prop: Arc<LocalV2rayProperty>,
    child: Arc<Mutex<Option<Child>>>,
    config: OnceCell<String>,
}

impl LocalV2ray {
    pub fn new(prop: LocalV2rayProperty) -> Self {
        Self {
            prop: Arc::new(prop),
            child: Arc::new(Mutex::new(None)),
            config: OnceCell::new(),
        }
    }
}

#[async_trait]
impl ConfigurableV2ray for LocalV2ray {
    async fn get_config(&self) -> Result<&str> {
        let config = self.config.get_or_try_init(|| {
            if let Some(path) = &self.prop.config_path {
                log::trace!("loading config from path: {}", path);
                fs::read_to_string(path)
            } else {
                log::debug!("loading config from custom");
                Ok(config::get_tcp_ping_config())
            }
        })?;
        Ok(config)
    }

    async fn apply_config(&self, nodes: &[&Node]) -> Result<String> {
        if let Ok(contents) = &self.get_config().await {
            config::apply_config(contents, nodes, None)
        } else {
            config::gen_load_balance_config(nodes, self.get_available_port().await?)
        }
    }
}

#[async_trait]
impl V2rayService for LocalV2ray {
    /// 从bin path中使用config启动v2ray 并返回子进程 由用户控制
    async fn start(&self, config: &str) -> Result<Child> {
        let mut child = Command::new(&self.prop.bin_path)
            .arg("-config")
            .arg("stdin:")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        // 写完stdin后drop避免阻塞
        child
            .stdin
            .take()
            .expect("stdin get error")
            .write_all(config.as_bytes())
            .await?;

        // 2. check start with output
        // 不能使用stdout.take(): error trying to connect: Connection reset by peer (os error 104)
        let out = child.stdout.as_mut().expect("not found child stdout");
        let mut reader = BufReader::new(out).lines();
        while let Some(line) = reader.next_line().await? {
            log::trace!("v2ray stdout line: {}", line);
            if line.contains("started") && line.contains("v2ray.com/core: V2Ray") {
                break;
            }
        }
        log::trace!("v2ray start successful");
        Ok(child)
    }

    async fn start_in_background(&self, config: &str) -> Result<()> {
        if let Some(old) = self.child.lock().await.as_ref() {
            log::error!(
                "start another instance in the background. old id: {:?}, new config: {}",
                old.id(),
                config
            );
            return Err(anyhow!(
                "unsupported multiple instances. old id: {:?}",
                old.id()
            ));
        }
        let child = self.start(config).await?;
        *self.child.lock().await = Some(child);
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        if let Some(mut child) = self.child.lock().await.take() {
            log::trace!("killing cached v2ray id: {:?}", child.id());
            child.kill().await?;
        }
        Ok(())
    }

    async fn stop_all(&self) -> Result<()> {
        self.stop().await?;

        let out = Command::new("killall")
            .arg("-9")
            .arg("v2ray")
            .output()
            .await?;
        if out.status.success() {
            log::debug!(
                "killall v2ray success: {}",
                String::from_utf8_lossy(&out.stdout)
            );
        } else {
            log::debug!(
                "killall v2ray failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    }

    async fn get_available_port(&self) -> Result<u16> {
        let listen = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listen.local_addr()?;
        Ok(addr.port())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Once;

    use log::LevelFilter;
    use once_cell::sync::Lazy;

    use super::*;

    #[tokio::test]
    async fn config() -> Result<()> {
        let local = localv2();
        let config = local.get_config().await?;
        log::debug!("config: {}", config);

        let ssh = sshv2();
        let config = ssh.get_config().await?;
        log::debug!("ssh config: {}", config);

        Ok(())
    }

    fn localv2() -> LocalV2ray {
        let content = r#"{}"#;
        let prop = serde_yaml::from_str::<LocalV2rayProperty>(content).unwrap();
        LocalV2ray::new(prop)
    }

    fn sshv2() -> SshV2ray {
        let content = r#"
        username: root
        host: 192.168.93.2
        config_path: /var/etc/ssrplus/tcp-only-ssr-retcp.json
        bin_path: /usr/bin/v2ray"#;
        let prop = serde_yaml::from_str::<SshV2rayProperty>(content).unwrap();
        SshV2ray::new(prop)
    }

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
                .init();
        });
    }

    fn get_node() -> Node {
        serde_json::from_str(
            r#"{
            "host": "hk02.az.jinkela.icu",
            "path": "/hls",
            "tls": "",
            "verify_cert": true,
            "add": "gz01.mobile.lay168.net",
            "port": 61022,
            "aid": 2,
            "net": "ws",
            "headerType": "none",
            "localserver": "hk02.az.jinkela.icu",
            "v": "2",
            "type": "vmess",
            "ps": "广州01→香港02 | 1.5x NF",
            "remark": "广州01→香港02 | 1.5x NF",
            "id": "55fb0457-d874-32c3-89a2-679fed6eabf1",
            "class": 1
        }"#,
        )
        .unwrap()
    }
}

// #[cfg(test)]
// mod tests {

//     use std::sync::Once;

//     use super::*;
//     use log::LevelFilter;

//     #[cfg(test)]
//     mod v2ray_tests {
//         use super::*;

//         #[tokio::test]
//         async fn tcp_ping() -> Result<()> {
//             let v2 = V2ray::new(Default::default());
//             let pp = Default::default();
//             let nodes = vec![get_node(), get_node(), get_node()];
//             let ps = v2.tcp_ping_nodes(nodes, &pp).await;
//             log::debug!("{:?}", ps);
//             Ok(())
//         }
//     }

//     #[tokio::test]
//     async fn restart_in_ssh_background_test() -> Result<()> {
//         async fn get_v2ray_out(username: &str, host: &str) -> Result<Option<String>> {
//             let cmd = format!("{}@{}", username, host);
//             let out = Command::new("ssh")
//                 .arg(&cmd)
//                 .arg("ps | grep v2ray | grep -v grep")
//                 .stdout(Stdio::piped())
//                 .output()
//                 .await?;
//             log::debug!("error: {}", String::from_utf8_lossy(&out.stderr));
//             let out = if out.status.success() {
//                 Some(String::from_utf8(out.stdout)?)
//             } else {
//                 None
//             };
//             Ok(out)
//         }
//         let (username, host) = ("root", "openwrt");
//         let old_out = get_v2ray_out(username, host).await?;

//         let node = get_node();
//         let config = gen_tcp_ping_config(&node, 1080)?;
//         restart_in_ssh_background(&config, username, host).await?;

//         let new_out = get_v2ray_out(username, host).await?;
//         assert_ne!(new_out, old_out);
//         Ok(())
//     }

//     #[tokio::test]
//     async fn get_config_ssh_test() -> Result<()> {
//         let (username, host, path) = (
//             "root",
//             "openwrt",
//             "/var/etc/ssrplus/tcp-only-ssr-retcp.json",
//         );
//         let config = get_config_ssh(username, host, path).await?;
//         assert!(config.contains(r#""port": 1234"#));
//         assert!(!config.contains(path));
//         Ok(())
//     }

//     #[tokio::test]
//     async fn get_config_ssh_parse_test() -> Result<()> {
//         let (username, host, path) = (
//             "root",
//             "openwrt",
//             "/var/etc/ssrplus/tcp-only-ssr-retcp.json",
//         );
//         let node = get_node();
//         let config = get_config_ssh(username, host, path).await?;

//         let config = apply_config(&config, &[&node], None)?;
//         assert!(config.contains(&node.add.unwrap()));
//         Ok(())
//     }

//     #[tokio::test]
//     async fn start_test() -> Result<()> {
//         let node = get_node();
//         let vp = V2rayProperty::default();
//         let config = gen_tcp_ping_config(&node, get_available_port().await?)?;
//         let bin_path = vp
//             .bin_path
//             .unwrap_or_else(|| find_bin_path("v2ray").unwrap());

//         start(&bin_path, &config).await?;
//         Ok(())
//     }

//     // #[test]
//     #[tokio::test]
//     async fn measure_duration_with_v2ray_start() -> Result<()> {
//         let node = get_node();
//         let vp = V2rayProperty::default();
//         let pp = PingProperty::default();

//         let local_port = get_available_port().await?;
//         let config = gen_tcp_ping_config(&node, local_port)?;
//         let bin_path = vp
//             .bin_path
//             .unwrap_or_else(|| find_bin_path("v2ray").unwrap());

//         let mut child = start(&bin_path, &config).await?;

//         measure_duration_with_proxy(&pp.ping_url, local_port, Duration::from_secs(2))
//             .await
//             .expect("get duration error");
//         child.kill().await?;
//         Ok(())
//     }

//     #[tokio::test]
//     async fn tcp_ping_test() -> Result<()> {
//         let node = get_node();
//         let vp = V2rayProperty::default();
//         let pp = PingProperty::default();
//         let local_port = get_available_port().await?;
//         let config = gen_tcp_ping_config(&node, local_port)?;
//         let bin_path = vp
//             .bin_path
//             .unwrap_or_else(|| find_bin_path("v2ray").unwrap());

//         let stats = tcp_ping(&bin_path, &config, local_port, &pp).await?;
//         assert_eq!(
//             stats.durations.len(),
//             PingProperty::default().count as usize
//         );
//         assert!(stats.durations.iter().filter(|d| d.is_some()).count() > 0);
//         Ok(())
//     }

//     #[tokio::test]
//     async fn tcp_ping_error_when_node_unavailable() -> Result<()> {
//         let mut node = get_node();
//         node.add = Some("test.host.addr".to_owned());

//         let vp = V2rayProperty::default();
//         let pp = PingProperty::default();
//         let local_port = get_available_port().await?;
//         let config = gen_tcp_ping_config(&node, local_port)?;
//         let bin_path = vp
//             .bin_path
//             .unwrap_or_else(|| find_bin_path("v2ray").unwrap());

//         let stats = tcp_ping(&bin_path, &config, local_port, &pp).await?;

//         assert_eq!(
//             stats.durations.len(),
//             PingProperty::default().count as usize
//         );
//         assert_eq!(stats.durations.iter().filter(|d| d.is_some()).count(), 0);
//         Ok(())
//     }

//     static INIT: Once = Once::new();

//     #[cfg(test)]
//     #[ctor::ctor]
//     fn init() {
//         INIT.call_once(|| {
//             env_logger::builder()
//                 .is_test(true)
//                 .filter_level(LevelFilter::Info)
//                 .filter_module("v2ray_monitor", LevelFilter::Trace)
//                 .init();
//         });
//     }

//     fn get_node() -> Node {
//         serde_json::from_str(
//             r#"{
//             "host": "hk02.az.jinkela.icu",
//             "path": "/hls",
//             "tls": "",
//             "verify_cert": true,
//             "add": "gz01.mobile.lay168.net",
//             "port": 61022,
//             "aid": 2,
//             "net": "ws",
//             "headerType": "none",
//             "localserver": "hk02.az.jinkela.icu",
//             "v": "2",
//             "type": "vmess",
//             "ps": "广州01→香港02 | 1.5x NF",
//             "remark": "广州01→香港02 | 1.5x NF",
//             "id": "55fb0457-d874-32c3-89a2-679fed6eabf1",
//             "class": 1
//         }"#,
//         )
//         .unwrap()
//     }
// }

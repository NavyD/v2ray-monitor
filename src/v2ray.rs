pub mod config;
pub mod node;

use crate::task::v2ray_task_config::*;
use async_trait::async_trait;

use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use regex::Regex;
use std::{collections::HashMap, fs, process::Stdio, sync::Arc};

use anyhow::{anyhow, Result};

use tokio::{
    io::*,
    net::TcpListener,
    process::{Child, Command},
};

use self::node::Node;

#[async_trait]
pub trait ConfigurableV2ray {
    async fn get_config(&self) -> Result<&str>;

    async fn gen_config(&self, nodes: &[&Node]) -> Result<String>;

    async fn gen_ping_config(&self, node: &Node, port: u16) -> Result<String> {
        let config = self.get_config().await?;
        Ok(config::apply_config(config, &[node], Some(port))?)
    }
}

#[async_trait]
pub trait V2rayService: Send + Sync + Clone + ConfigurableV2ray + 'static {
    /// 启动v2ray并返回对应的v2ray进程，由调用者控制进程
    async fn start(&self, config: &str) -> Result<Child>;

    /// 在后台启动v2ray，允许重用和控制停止
    async fn start_in_background(&self, config: &str) -> Result<u32>;

    /// 停止指定port上的v2ray进程。如果不存在也不会返回错误
    async fn stop(&self, port: u16) -> Result<()>;

    async fn stop_all(&self) -> Result<()>;

    async fn get_available_port(&self) -> Result<u16>;

    async fn clean_env(&self) -> Result<()>;

    fn get_running_ports(&self) -> Option<Vec<u16>>;

    fn is_running(&self, port: u16) -> bool;

    fn get_host(&self) -> &str;

    fn get_proxy_url(&self, config: &str) -> Result<Option<String>> {
        config::get_proxy_url(config, self.get_host()).map(Some)
    }

    async fn restart_in_background(&self, config: &str) -> Result<u32> {
        let port = config::get_port(config)?;
        self.stop(port).await?;
        self.start_in_background(config).await
    }

    async fn clean_start_in_background(&self, config: &str) -> Result<()> {
        self.clean_env().await?;
        self.stop_all().await?;
        self.start_in_background(config).await?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct SshV2ray {
    prop: Arc<SshV2rayProperty>,
    config: OnceCell<String>,
    port_pids: Arc<Mutex<HashMap<u16, u32>>>,
    pid_regex: Regex,
}

impl SshV2ray {
    pub fn new(prop: SshV2rayProperty) -> Self {
        Self {
            prop: Arc::new(prop),
            config: OnceCell::new(),
            port_pids: Arc::new(Mutex::new(HashMap::new())),
            pid_regex: Regex::new(r"(\d+)\s+running").unwrap(),
        }
    }

    fn ssh_addr(&self) -> String {
        format!("{}@{}", self.prop.username, self.prop.host)
    }

    async fn ssh_exe(&self, sh_cmd: &str) -> Result<String> {
        exe_arg(
            &format!("ssh {}@{}", self.prop.username, self.prop.host),
            sh_cmd,
        )
        .await
    }

    fn parse_pid_from_jobs_l(&self, s: &str) -> Result<u32> {
        let caps = self
            .pid_regex
            .captures(&s)
            .ok_or_else(|| anyhow!("not matched pid regex: {} for input: {}", self.pid_regex, s))?;
        if caps.len() != 2 {
            log::error!(
                "pid regex {} found multiple configuration items: {:?}",
                self.pid_regex,
                caps,
            );
            return Err(anyhow!("pid regex found multiple configuration items"));
        }
        Ok(caps[1].parse::<u32>()?)
    }
}

#[async_trait]
impl ConfigurableV2ray for SshV2ray {
    /// 使用`scp username@host:path`读取配置到内存中
    ///
    /// # Errors
    ///
    /// * 如果命令执行失败
    async fn get_config(&self) -> Result<&str> {
        if let Some(s) = self.config.get() {
            return Ok(s);
        }
        // scp get content
        let sh_cmd = format!(
            "scp {}@{}:{} /dev/stdout",
            self.prop.username, self.prop.host, self.prop.config_path
        );
        log::trace!("loading config from ssh command: {}", sh_cmd);
        let args = sh_cmd.split(' ').collect::<Vec<_>>();
        let out = Command::new(args[0]).args(&args[1..]).output().await?;
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr);
            log::error!("get config from ssh {} error: {}", sh_cmd, msg);
            return Err(anyhow!("get config ssh error: {}", msg));
        }
        Ok(self
            .config
            .get_or_try_init(|| String::from_utf8(out.stdout))?)
    }

    async fn gen_config(&self, nodes: &[&Node]) -> Result<String> {
        let contents = self.get_config().await?;
        config::apply_config(&contents, nodes, None)
    }
}

#[async_trait]
impl V2rayService for SshV2ray {
    async fn start(&self, config: &str) -> Result<Child> {
        let sh_cmd = format!("echo '{}' | v2ray -config stdin:", config);
        log::trace!("starting with ssh command: {}", sh_cmd);
        let mut child = Command::new("ssh")
            .arg(&format!("{}@{}", self.prop.username, self.prop.host))
            .arg(&sh_cmd)
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        check_v2ray_start(
            child
                .stdout
                .as_mut()
                .ok_or_else(|| anyhow!("not found stdout"))?,
        )
        .await?;
        Ok(child)
    }

    async fn start_in_background(&self, config: &str) -> Result<u32> {
        // check if port exists
        let port = config::get_port(config)?;
        if let Some(pid) = self.port_pids.lock().get(&port) {
            log::error!("found v2ray process {} started in port {}", pid, port);
            return Err(anyhow!("duplicate v2ray process in port: {}", port));
        }
        let sh_cmd = format!(
            "echo '{}' | nohup v2ray -config stdin: &> /dev/null &; jobs -l",
            config,
        );
        let out = exe_arg(&format!("ssh {}", self.ssh_addr()), &sh_cmd).await?;
        let pid = self.parse_pid_from_jobs_l(&out)?;
        self.port_pids.lock().insert(port, pid);
        log::debug!(
            "v2ray successfully started in the background. pid: {}, port: {}",
            pid,
            port
        );
        Ok(pid)
    }

    async fn stop(&self, port: u16) -> Result<()> {
        // 可以在background时读取进程id保存，然后ssh kill
        let pid = self.port_pids.lock().remove(&port);
        if let Some(pid) = pid {
            if let Err(e) = self.ssh_exe(&format!("kill -9 {}", pid)).await {
                log::info!("stop v2ray {} failed: {}", pid, e);
            } else {
                log::debug!("successfully kill v2ray process id: {}", pid);
            }
        }
        Ok(())
    }

    async fn stop_all(&self) -> Result<()> {
        // clean ssr-monitor and killall
        // let sh_cmd = {
        //     let kill_ssr_monitor = "ps -ef | grep ssr-monitor | grep -v grep | awk '{print $1}' | xargs kill -9 && echo 'killed ssr-monitor on busbox'";
        //     let kill_v2ray = "killall -9 v2ray && echo 'killed v2ray'";
        //     format!("{};{};", kill_ssr_monitor, kill_v2ray)
        // };

        let pids =
            self.port_pids
                .lock()
                .drain()
                .map(|(_, pid)| pid)
                .fold(String::new(), |mut s, id| {
                    s.push_str(&id.to_string());
                    s.push(' ');
                    s
                });
        if !pids.is_empty() {
            self.ssh_exe(&format!("kill -9 {}", pids)).await?;
            if log::log_enabled!(log::Level::Debug) {
                log::debug!(
                    "successfully kill v2ray processes: {}, pids: {}",
                    pids.split(' ').count(),
                    pids
                );
            }
        }
        Ok(())
    }

    async fn clean_start_in_background(&self, config: &str) -> Result<()> {
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
        unimplemented!()
    }

    fn get_host(&self) -> &str {
        &self.prop.host
    }

    fn is_running(&self, port: u16) -> bool {
        self.port_pids.lock().contains_key(&port)
    }

    async fn clean_env(&self) -> Result<()> {
        let sh_cmd = {
            let kill_ssr_monitor = "ps -ef | grep ssr-monitor | grep -v grep | awk '{print $1}' | xargs kill -9 && echo 'killed ssr-monitor on busbox'";
            let kill_v2ray = "killall -9 v2ray && echo 'killed v2ray'";
            format!("{};{};", kill_ssr_monitor, kill_v2ray)
        };
        log::trace!("killing ssr-monitor and v2ray for clean env");
        if let Err(e) = self.ssh_exe(&sh_cmd).await {
            if !e.to_string().contains("no process killed") {
                log::error!("execute clean env error: {}", e);
                return Err(e);
            }
        }
        Ok(())
    }

    fn get_running_ports(&self) -> Option<Vec<u16>> {
        let pp = self.port_pids.lock();
        if pp.is_empty() {
            None
        } else {
            Some(pp.keys().copied().collect())
        }
    }

    /// 使用路由全局自动代理
    fn get_proxy_url(&self, _config: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

#[derive(Clone)]
pub struct LocalV2ray {
    prop: Arc<LocalV2rayProperty>,
    port_children: Arc<Mutex<HashMap<u16, Child>>>,
    config: OnceCell<String>,
}

impl LocalV2ray {
    pub fn new(prop: LocalV2rayProperty) -> Self {
        Self {
            prop: Arc::new(prop),
            port_children: Arc::new(Mutex::new(HashMap::new())),
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
                let config = config::get_tcp_ping_config();
                log::trace!("loading config from custom");
                Ok(config)
            }
        })?;
        Ok(config)
    }

    async fn gen_config(&self, nodes: &[&Node]) -> Result<String> {
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
        if log::log_enabled!(log::Level::Trace) {
            let port = config::get_port(config)?;
            log::trace!("starting v2ray on port {}", port);
        }
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
        check_v2ray_start(
            child
                .stdout
                .as_mut()
                .ok_or_else(|| anyhow!("not found command stdout"))?,
        )
        .await?;
        log::trace!("v2ray start successful");
        Ok(child)
    }

    async fn start_in_background(&self, config: &str) -> Result<u32> {
        // check duplicate v2ray in port
        let port = config::get_port(config)?;
        if let Some(child) = self.port_children.lock().get(&port) {
            log::error!(
                "found v2ray process {:?} started in port {}",
                child.id(),
                port
            );
            return Err(anyhow!("duplicate v2ray process in port: {}", port));
        }
        // start v2ray
        let child = self.start(config).await?;
        let pid = child.id().ok_or_else(|| anyhow!("not found process id"))?;
        if log::log_enabled!(log::Level::Debug) {
            log::debug!("successfully start v2ray process {} on port {}", pid, port);
        }
        self.port_children.lock().insert(port, child);
        Ok(pid)
    }

    async fn stop(&self, port: u16) -> Result<()> {
        let mut child = self.port_children.lock().remove(&port);
        if let Some(child) = child.as_mut() {
            log::debug!(
                "killing cached v2ray id: {:?} in port: {}",
                child.id(),
                port
            );
            child.kill().await?;
        } else {
            log::debug!("no v2ray process in port: {}", port);
        }
        Ok(())
    }

    async fn stop_all(&self) -> Result<()> {
        // self.stop().await?;
        let mut pcs = self.port_children.lock().drain().collect::<Vec<_>>();
        for (p, c) in &mut pcs {
            log::trace!("killing v2ray {:?} in port: {}", c.id(), p);
            c.kill().await?;
        }
        log::debug!("successfully killed {} v2ray processes", pcs.len());
        Ok(())
    }

    async fn get_available_port(&self) -> Result<u16> {
        let listen = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listen.local_addr()?;
        Ok(addr.port())
    }

    fn get_host(&self) -> &str {
        "127.0.0.1"
    }

    fn is_running(&self, port: u16) -> bool {
        self.port_children.lock().contains_key(&port)
    }

    async fn clean_env(&self) -> Result<()> {
        log::trace!("killing all v2ray for clean env");
        if let Err(e) = exe("killall -9 v2ray").await {
            if !e.to_string().contains("v2ray: no process found") {
                return Err(e);
            }
        }
        Ok(())
    }

    fn get_running_ports(&self) -> Option<Vec<u16>> {
        let pc = self.port_children.lock();
        if pc.is_empty() {
            None
        } else {
            Some(pc.keys().copied().collect())
        }
    }
}

async fn check_v2ray_start(out: &mut tokio::process::ChildStdout) -> Result<()> {
    // 2. check start with output
    // 不能使用stdout.take(): error trying to connect: Connection reset by peer (os error 104)
    let mut reader = BufReader::new(out).lines();
    while let Some(line) = reader.next_line().await? {
        log::trace!("v2ray: {}", line);
        // 兼容xray: 2021/02/15 16:40:29 [Warning] core: Xray 1.2.4 started
        if line.contains("Warning") && line.contains("ay") && line.contains("started") {
            return Ok(());
        }
    }
    Err(anyhow!("v2ray start has some problem"))
}

async fn exe(cmd: &str) -> Result<String> {
    exe_arg(cmd, "").await
}

async fn exe_arg(command: &str, extra_arg: &str) -> Result<String> {
    if command.is_empty() {
        return Err(anyhow!("empty cmd"));
    }
    let cmd = command.split(' ').collect::<Vec<_>>();
    log::trace!("executing command: {} '{:?}'", command, extra_arg);
    let mut t_cmd = Command::new(cmd[0]);
    t_cmd.args(&cmd[1..]);
    if !extra_arg.is_empty() {
        t_cmd.arg(extra_arg);
    }
    let out = t_cmd.stdout(Stdio::piped()).output().await?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        log::debug!(
            "execute command failed for {} '{:?}', error: {}",
            command,
            extra_arg,
            msg
        );
        Err(anyhow!("execute error: {}", msg))
    } else {
        let s = String::from_utf8(out.stdout)?;
        log::trace!("execute success: {}", s);
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Once, time::Duration};

    use log::LevelFilter;
    use tokio::time::timeout;

    use crate::task::find_v2ray_bin_path;

    use super::*;

    #[tokio::test]
    async fn load_config_from_path() -> Result<()> {
        async fn test(v2: impl V2rayService) -> Result<()> {
            let config = v2.get_config().await?;
            assert!(!config.is_empty());
            // user id
            assert!(config.contains("55fb0457-d874-32c3-89a2-679fed6eabf1"));
            Ok(())
        }

        test(LocalV2ray::new(get_local_prop()?)).await?;
        test(SshV2ray::new(get_ssh_prop()?)).await?;

        Ok(())
    }

    #[tokio::test]
    async fn wait_after_start() -> Result<()> {
        async fn test(v2: impl V2rayService) -> Result<()> {
            let config = v2.get_config().await?;
            let mut child = v2.start(config).await?;
            // child不应该没有退出
            let v = timeout(Duration::from_millis(100), child.wait()).await;
            assert!(v.is_err());
            Ok(())
        }

        test(LocalV2ray::new(get_local_prop()?)).await?;
        test(SshV2ray::new(get_ssh_prop()?)).await?;
        Ok(())
    }

    #[tokio::test]
    async fn local_start_in_background_stop_all() -> Result<()> {
        let v2 = LocalV2ray::new(get_local_prop()?);
        async fn exe_arg(cmd: String, arg: String) -> Result<String> {
            crate::v2ray::exe_arg(&cmd, &arg).await
        }
        let grep = |key: &str| {
            exe_arg(
                "sh -c".to_string(),
                format!("ps -ef | grep {} | grep -v grep", key),
            )
        };
        let grep_wc = |key: &str| {
            exe_arg(
                "sh -c".to_string(),
                format!("ps -ef | grep {} | grep -v grep | wc -l", key),
            )
        };

        let first = grep_wc("v2ray").await?;
        // bg run
        let pid = v2.start_in_background(v2.get_config().await?).await?;

        // check pid
        assert!(&grep(&pid.to_string()).await.is_ok());
        assert!(&grep(&(pid * 17).to_string()).await.is_err());
        // run check
        let sec = grep_wc("v2ray").await?;
        assert_ne!(first, sec);

        // stop all
        v2.stop_all().await?;
        let third = grep_wc("v2ray").await?;
        // stop check
        assert_ne!(third, sec);
        assert_eq!(third, first);
        Ok(())
    }

    #[tokio::test]
    async fn start_in_ssh_background_stop_all() -> Result<()> {
        let v2 = SshV2ray::new(get_ssh_prop()?);
        async fn exe_arg(cmd: String, arg: String) -> Result<String> {
            crate::v2ray::exe_arg(&cmd, &arg).await
        }
        let grep = |key: &str| {
            exe_arg(
                format!("ssh {}", v2.ssh_addr()),
                format!("ps -ef | grep {} | grep -v grep", key),
            )
        };
        let grep_wc = |key: &str| {
            exe_arg(
                format!("ssh {}", v2.ssh_addr()),
                format!("ps -ef | grep {} | grep -v grep | wc -l", key),
            )
        };

        let first = grep_wc("v2ray").await?;
        // bg run
        let pid = v2.start_in_background(v2.get_config().await?).await?;

        // check pid
        assert!(&grep(&pid.to_string()).await.is_ok());
        assert!(&grep(&(pid * 17).to_string()).await.is_err());
        // run check
        let sec = grep_wc("v2ray").await?;
        assert_ne!(first, sec);

        // stop all
        v2.stop_all().await?;
        let third = grep_wc("v2ray").await?;
        // stop check
        assert_ne!(third, sec);
        assert_eq!(third, first);
        Ok(())
    }

    #[tokio::test]
    async fn stop_after_start_in_background() -> Result<()> {
        async fn test(v2: impl V2rayService) -> Result<()> {
            assert!(v2.stop(61110).await.is_ok());
            let config = v2.get_config().await?;
            let port = config::get_port(config)?;
            v2.start_in_background(config).await?;
            assert!(v2.stop(port).await.is_ok());
            Ok(())
        }
        test(LocalV2ray::new(get_local_prop()?)).await?;
        test(SshV2ray::new(get_ssh_prop()?)).await?;
        Ok(())
    }

    #[tokio::test]
    async fn get_port() -> Result<()> {
        async fn test(v2: impl V2rayService) -> Result<()> {
            let port = v2.get_available_port().await?;
            assert!(!v2.is_running(port));
            Ok(())
        }
        test(LocalV2ray::new(get_local_prop()?)).await?;
        // test(SshV2ray::new(get_ssh_prop()?)).await?;

        Ok(())
    }

    #[tokio::test]
    async fn clean_test() -> Result<()> {
        async fn test(v2: impl V2rayService) -> Result<()> {
            v2.clean_env().await?;
            Ok(())
        }
        test(LocalV2ray::new(get_local_prop()?)).await?;
        test(SshV2ray::new(get_ssh_prop()?)).await?;
        Ok(())
    }

    #[test]
    fn parse_regex_test() -> Result<()> {
        let content = r#" [1]  + 219403 done       echo  | 
        219404 running    nohup v2ray -config stdin: &> /dev/null"#;
        let v2 = SshV2ray::new(get_ssh_prop()?);
        assert_eq!(v2.parse_pid_from_jobs_l(content)?, 219404);
        Ok(())
    }

    fn get_local_prop() -> Result<LocalV2rayProperty> {
        Ok(LocalV2rayProperty {
            bin_path: find_v2ray_bin_path()?,
            config_path: Some(
                Path::new("tests/data")
                    .join("local-v2-config.json")
                    .to_string_lossy()
                    .to_string(),
            ),
        })
    }

    fn get_ssh_prop() -> Result<SshV2rayProperty> {
        let content = r#"
username: root
host: 192.168.93.2
config_path: /var/etc/ssrplus/tcp-only-ssr-retcp.json
bin_path: /usr/bin/v2ray
"#;
        Ok(serde_yaml::from_str::<SshV2rayProperty>(content)?)
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
                .filter_module(crate_name, LevelFilter::Debug)
                // .filter_module(&(crate_name.to_string() + "::tcp_ping"), LevelFilter::Debug)
                .init();
            // TEST_DIR.set(Path::new("tests/data").to_path_buf());
        });
    }

    // fn get_node() -> Node {
    //     serde_json::from_str(
    //         r#"{
    //         "host": "hk02.az.jinkela.icu",
    //         "path": "/hls",
    //         "tls": "",
    //         "verify_cert": true,
    //         "add": "gz01.mobile.lay168.net",
    //         "port": 61022,
    //         "aid": 2,
    //         "net": "ws",
    //         "headerType": "none",
    //         "localserver": "hk02.az.jinkela.icu",
    //         "v": "2",
    //         "type": "vmess",
    //         "ps": "广州01→香港02 | 1.5x NF",
    //         "remark": "广州01→香港02 | 1.5x NF",
    //         "id": "55fb0457-d874-32c3-89a2-679fed6eabf1",
    //         "class": 1
    //     }"#,
    //     )
    //     .unwrap()
    // }
}

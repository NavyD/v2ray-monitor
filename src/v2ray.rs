pub mod config;
pub mod node;

use crate::task::v2ray_task_config::*;
use async_trait::async_trait;

use double_checked_cell_async::DoubleCheckedCell;
use once_cell::sync::OnceCell;
use regex::Regex;
use std::{collections::HashMap, path::Path, process::Stdio, sync::Arc, time::Duration};

use anyhow::{anyhow, Result};
use tokio::{sync::Mutex, time};

use tokio::{
    fs::read_to_string,
    io::*,
    net::TcpListener,
    process::{Child, Command},
};

use self::node::Node;

/// 控制v2ray启动与停止的服务。
///
/// 由于rust当前不支持async drop，出于资源的限制，对于ssh后台启动时不能保持
/// 一个child，不能自动停止已启动的v2ray，需要主动调用`stop_all`
#[async_trait]
pub trait V2rayService: Send + Sync {
    /// 获取启动v2ray的配置。
    async fn get_config(&self) -> Result<&str>;

    /// 在后台启动v2ray并返回其进程pid
    async fn start_in_background(&self, config: &str) -> Result<u32>;

    /// 停止指定port上的v2ray进程。如果不存在返回false
    async fn stop_by_port(&self, port: &u16) -> Result<bool>;

    /// 根据pid停止v2ray进程。如果不存在返回false
    async fn stop_by_pid(&self, pid: &u32) -> Result<bool>;

    /// 停止所有由当前实例启动的v2ray进程
    async fn stop_all(&self) -> Result<()>;

    /// 返回一个当前系统可用的端口，用于在config时设置，任意的端口可能导致在tcp ping启动时
    /// 出现端口占用冲突
    async fn get_available_port(&self) -> Result<u16>;

    /// 清理当前的系统环境，用于保证v2ray启动成功，不会频繁的调用
    async fn clean_env(&self) -> Result<()>;

    /// 在系统中判断指定v2ray pid是否存在。外部环境可能会变化
    /// 不应该使用缓存
    async fn is_running(&self, pid: u32) -> Result<bool>;

    fn get_host(&self) -> &str;

    /// 重启指定config上端口对应的v2ray进程
    async fn restart_in_background(&self, config: &str) -> Result<u32> {
        let port = config::get_port(config)?;
        self.stop_by_port(&port).await?;
        self.start_in_background(config).await
    }

    /// 生成nodes对应的v2ray负载均衡配置。这是一个便利方法
    async fn gen_config(&self, nodes: &[&Node]) -> Result<String> {
        let contents = self.get_config().await?;
        config::apply_config(contents, nodes, None)
    }
}

/// 通过检查v2ray的warning日志查看v2ray是否启动。如：`[Warning] core: Xray 1.2.4 started`
///
/// 如果在 `timeout` 后没有检查到输出started日志返回err
async fn check_v2ray_start(out: &mut tokio::process::ChildStdout, timeout: Duration) -> Result<()> {
    // 不能使用stdout.take(): error trying to connect: Connection reset by peer (os error 104)
    let mut reader = BufReader::new(out).lines();
    time::timeout(timeout, async {
        loop {
            match reader.next_line().await {
                Ok(line) => {
                    if let Some(line) = line {
                        log::trace!("v2ray: {}", line);
                        // 兼容xray: 2021/02/15 16:40:29 [Warning] core: Xray 1.2.4 started
                        if line.contains("Warning")
                            && line.contains("ay")
                            && line.contains("started")
                        {
                            return Ok(());
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
    })
    .await??;
    Ok(())
}

async fn exe(cmd: &str) -> Result<String> {
    exe_arg(cmd, "", None).await
}

async fn exe_arg(
    command: &str,
    extra_arg: &str,
    allow_err_status: Option<&[i32]>,
) -> Result<String> {
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
        let errmsg = String::from_utf8(out.stderr)?;
        log::trace!(
            "execute command `{}` failed: status: {}, stderr: {:?}",
            command,
            out.status,
            errmsg
        );
        if let Some(status) = allow_err_status {
            if status.contains(&out.status.code().unwrap()) {
                return Ok(errmsg);
            }
        }
        Err(anyhow!(
            "execute error: status: {}, msg: {}",
            out.status,
            errmsg
        ))
    } else {
        let s = String::from_utf8(out.stdout)?;
        log::trace!("execute success: {}", s);
        Ok(s)
    }
}

pub struct LocalV2rayService {
    config: DoubleCheckedCell<String>,
    port_children: Arc<Mutex<HashMap<u16, Child>>>,
    prop: LocalV2rayProperty,
}

impl LocalV2rayService {
    pub fn new(prop: LocalV2rayProperty) -> Self {
        Self {
            prop,
            config: DoubleCheckedCell::new(),
            port_children: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 从bin path中使用config启动v2ray 并返回子进程 由用户控制
    async fn start(&self, config: &str) -> Result<Child> {
        if log::log_enabled!(log::Level::Trace) {
            let port = config::get_port(config)?;
            log::trace!("starting v2ray on port {}", port);
        }
        let mut child = tokio::process::Command::new(&self.prop.bin_path)
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
            Duration::from_secs(2),
        )
        .await?;
        log::trace!("v2ray start successful");
        Ok(child)
    }
}

#[async_trait]
impl V2rayService for LocalV2rayService {
    async fn get_config(&self) -> Result<&str> {
        self.config
            .get_or_try_init(async {
                let path = self.prop.config_path.as_ref().unwrap();
                log::debug!("loading config from local path: {}", path);
                read_to_string(path).await
            })
            .await
            .map(String::as_str)
            .map_err(Into::into)
    }

    async fn start_in_background(&self, config: &str) -> Result<u32> {
        // check duplicate v2ray in port
        let port = config::get_port(config)?;
        let mut lock = self.port_children.lock().await;
        if let Some(child) = lock.get(&port) {
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
        log::debug!("successfully start v2ray process {} on port {}", pid, port);
        lock.insert(port, child);
        Ok(pid)
    }

    async fn stop_by_port(&self, port: &u16) -> Result<bool> {
        log::trace!("stopping by port: {}", port);
        let mut child = self.port_children.lock().await.remove(&port);
        if let Some(child) = child.as_mut() {
            log::trace!(
                "killing cached v2ray id: {:?} in port: {}",
                child.id(),
                port
            );
            child.kill().await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn stop_by_pid(&self, pid: &u32) -> Result<bool> {
        let port = self
            .port_children
            .lock()
            .await
            .iter()
            .find(|(_, v)| v.id() == Some(*pid))
            .map(|(k, _)| *k);
        if let Some(port) = port {
            self.stop_by_port(&port).await
        } else {
            Ok(false)
        }
    }

    async fn stop_all(&self) -> Result<()> {
        for (port, mut child) in self.port_children.lock().await.drain() {
            log::debug!(
                "killing cached v2ray id: {:?} in port: {}",
                child.id(),
                port
            );
            child.kill().await?;
        }
        Ok(())
    }

    async fn get_available_port(&self) -> Result<u16> {
        log::trace!("getting port from local system");
        Ok(TcpListener::bind("127.0.0.1:0").await?.local_addr()?.port())
    }

    async fn clean_env(&self) -> Result<()> {
        log::debug!("killing all v2ray for clean env");
        if let Err(e) = exe("killall -9 v2ray").await {
            if !e.to_string().contains("v2ray: no process found") {
                return Err(e);
            }
        }
        Ok(())
    }

    fn get_host(&self) -> &str {
        "127.0.0.1"
    }

    async fn is_running(&self, pid: u32) -> Result<bool> {
        Ok(Path::new(&format!("/proc/{}", pid)).exists())
        // exe(&format!("kill -0 {}", pid)).await.map(|s| !s.is_empty())
    }
}

pub struct SshV2rayService {
    config: DoubleCheckedCell<String>,
    prop: SshV2rayProperty,
    port_pids: tokio::sync::Mutex<HashMap<u16, u32>>,
    pid_regex: OnceCell<Regex>,
}

impl SshV2rayService {
    pub fn new(prop: SshV2rayProperty) -> Self {
        Self {
            prop,
            config: DoubleCheckedCell::new(),
            port_pids: tokio::sync::Mutex::new(HashMap::new()),
            pid_regex: OnceCell::new(),
        }
    }

    fn ssh_addr(&self) -> String {
        format!("{}@{}", self.prop.username, self.prop.host)
    }

    async fn ssh_exe(&self, sh_cmd: &str) -> Result<String> {
        exe_arg(
            &format!("ssh {}@{}", self.prop.username, self.prop.host),
            sh_cmd,
            None,
        )
        .await
    }

    /// 从`jobs -l`的输出中解析出当前shell后台运行的pid
    fn parse_pid_from_jobs_l(&self, s: &str) -> Result<u32> {
        let regex = self
            .pid_regex
            .get_or_try_init(|| Regex::new(r"(\d+)\s+running"))?;
        let caps = regex
            .captures(&s)
            .ok_or_else(|| anyhow!("not matched pid regex: {} for input: {}", regex, s))?;
        if caps.len() != 2 {
            log::error!(
                "pid regex {} found multiple configuration items: {:?}",
                regex,
                caps,
            );
            return Err(anyhow!("pid regex found multiple configuration items"));
        }
        caps[1].parse::<u32>().map_err(Into::into)
    }
}

#[async_trait]
impl V2rayService for SshV2rayService {
    async fn get_config(&self) -> Result<&str> {
        self.config
            .get_or_try_init(async {
                let sh_cmd = format!(
                    "scp {}@{}:{} /dev/stdout",
                    self.prop.username, self.prop.host, self.prop.config_path
                );
                log::debug!("loading config from ssh command: {}", sh_cmd);
                exe(&sh_cmd).await
            })
            .await
            .map(String::as_str)
    }

    async fn start_in_background(&self, config: &str) -> Result<u32> {
        // check if port exists
        let port = config::get_port(config)?;
        let mut guard = self.port_pids.lock().await;
        if let Some(pid) = guard.get(&port) {
            log::error!("found v2ray process {} started in port {}", pid, port);
            return Err(anyhow!("duplicate v2ray process in port: {}", port));
        }
        let sh_cmd = format!(
            "echo '{}' | nohup v2ray -config stdin: &> /dev/null &; jobs -l",
            config,
        );
        let out = exe_arg(&format!("ssh {}", self.ssh_addr()), &sh_cmd, None).await?;
        let pid = self.parse_pid_from_jobs_l(&out)?;
        guard.insert(port, pid);
        log::debug!(
            "v2ray successfully started in the background. pid: {}, port: {}",
            pid,
            port
        );
        Ok(pid)
    }

    async fn stop_by_port(&self, port: &u16) -> Result<bool> {
        // 避免死锁 `if let Some(pid) = self.port_pids.lock().await.get(port)`将导致self.stop_by_pid时不释放锁
        let port = self.port_pids.lock().await.get(port).copied();
        if let Some(pid) = port {
            self.stop_by_pid(&pid).await
        } else {
            Ok(false)
        }
    }

    /// 在background时读取进程id保存，然后ssh kill
    async fn stop_by_pid(&self, pid: &u32) -> Result<bool> {
        let mut lock = self.port_pids.lock().await;
        if let Some((port, pid)) = lock.iter().find(|(_, v)| *v == pid).map(|(k, v)| (*k, *v)) {
            let sh_cmd = format!("kill -9 {}", pid);

            let res = exe_arg(
                &format!("ssh {}@{}", self.prop.username, self.prop.host),
                &sh_cmd,
                Some(&[1]),
            )
            .await;

            if let Err(e) = res {
                Err(anyhow!("stop v2ray {} failed: {} on port {}", pid, e, port))
            } else {
                lock.remove(&port);
                log::debug!(
                    "successfully kill v2ray process id: {} on port: {}",
                    pid,
                    port
                );
                Ok(true)
            }
        } else {
            Ok(false)
        }
    }

    /// 使用`kill -9 pid1 pid2 ...`批量停止已启动的v2ray进程
    async fn stop_all(&self) -> Result<()> {
        let args = self
            .port_pids
            .lock()
            .await
            .drain()
            .map(|(_, pid)| pid.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        if args.is_empty() {
            return Ok(());
        }
        let cmd = format!("kill -9 {}", args);
        log::debug!("Killing all v2ray processes `{}`", cmd);
        self.ssh_exe(&cmd).await?;
        Ok(())
    }

    async fn get_available_port(&self) -> Result<u16> {
        unimplemented!()
    }

    async fn clean_env(&self) -> Result<()> {
        let sh_cmd = {
            let kill_ssr_monitor = "ps -ef | grep ssr-monitor | grep -v grep | awk '{print $1}' | xargs kill -9 && echo 'killed ssr-monitor on busbox'";
            let kill_v2ray = "killall -9 v2ray && echo 'killed v2ray'";
            format!("{};{};", kill_ssr_monitor, kill_v2ray)
        };
        log::debug!("killing ssr-monitor and v2ray for clean env");
        if let Err(e) = self.ssh_exe(&sh_cmd).await {
            if !e.to_string().contains("no process killed") {
                log::error!("execute clean env error: {}", e);
                return Err(e);
            }
        }
        Ok(())
    }

    fn get_host(&self) -> &str {
        &self.prop.host
    }

    async fn is_running(&self, pid: u32) -> Result<bool> {
        match self
            .ssh_exe(&format!("kill -0 {}", pid))
            .await
            .map(|s| s.is_empty())
        {
            Ok(v) => Ok(v),
            Err(e) => {
                if e.to_string().contains("no such process") {
                    Ok(false)
                } else {
                    Err(e)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use once_cell::sync::Lazy;

    use crate::task::find_v2ray_bin_path;

    use super::*;

    #[tokio::test]
    async fn load_config() -> Result<()> {
        async fn test(v2: &impl V2rayService) -> Result<()> {
            let (prev, cur, _) =
                tokio::try_join!(v2.get_config(), v2.get_config(), v2.get_config())?;
            assert_eq!(prev, cur);
            Ok(())
        };
        test(&ssh_v2()).await?;
        test(&local_v2()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn stop_all_after_start() -> Result<()> {
        async fn test(v2: &impl V2rayService) -> Result<()> {
            let config = v2.get_config().await?;
            let config1 = config::apply_port(config, 61002)?;
            let (p1, p2) = tokio::try_join!(
                v2.start_in_background(config),
                v2.start_in_background(&config1)
            )?;
            assert_eq!(v2.is_running(p1).await?, true);
            assert_eq!(v2.is_running(p2).await?, true);
            v2.stop_all().await?;
            assert_eq!(v2.is_running(p1).await?, false);
            assert_eq!(v2.is_running(p2).await?, false);
            Ok(())
        };
        test(&ssh_v2()).await?;
        test(&local_v2()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_start_failed_duplicate() -> Result<()> {
        async fn test(v2: &impl V2rayService) -> Result<()> {
            let v2 = Arc::new(v2);
            let config = v2.get_config().await?;
            let res = tokio::try_join!(
                v2.start_in_background(config),
                v2.start_in_background(config),
                v2.start_in_background(config),
            );
            v2.stop_all().await?;
            assert!(res.is_err());
            assert!(res.unwrap_err().to_string().contains("duplicate v2ray"));
            Ok(())
        }
        test(&ssh_v2()).await?;
        test(&local_v2()).await?;
        Ok(())
    }

    // #[tokio::test]
    // async fn killall_v2ray_when_ssh_v2ray_service_drop() -> Result<()> {
    //     let pid = {
    //         let v2 = ssh_v2();
    //         let config = v2.get_config().await?;
    //         let pid = v2.start_in_background(config).await?;
    //         assert!(v2.is_running(pid).await?);
    //         pid
    //     };
    //     assert!(!ssh_v2().is_running(pid).await?);
    //     Ok(())
    // }

    #[tokio::test]
    async fn stop_pid_normal() -> Result<()> {
        async fn test(v2: &impl V2rayService) -> Result<()> {
            let config = v2.get_config().await?;
            let pid = v2.start_in_background(config).await?;
            assert!(v2.is_running(pid).await?);
            v2.stop_by_pid(&pid).await?;
            assert!(!v2.is_running(pid).await?);
            Ok(())
        }
        test(&ssh_v2()).await?;
        test(&local_v2()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn get_local_port() -> Result<()> {
        let v2 = local_v2();
        let port = v2.get_available_port().await?;
        let config = v2.get_config().await?;
        let config = config::apply_port(config, port)?;
        let pid = v2.start_in_background(&config).await?;
        assert_eq!(v2.is_running(pid).await?, true);
        v2.stop_by_pid(&pid).await?;
        assert_eq!(v2.is_running(pid).await?, false);
        Ok(())
    }

    fn ssh_v2() -> SshV2rayService {
        SshV2rayService::new(SSH_PROP.clone())
    }

    fn local_v2() -> LocalV2rayService {
        LocalV2rayService::new(LOCAL_PROP.clone())
    }

    static SSH_PROP: Lazy<SshV2rayProperty> = Lazy::new(|| {
        let content = r#"
    username: root
    host: 192.168.93.2
    config_path: /var/etc/ssrplus/tcp-only-ssr-retcp.json
    bin_path: /usr/bin/v2ray
            "#;
        serde_yaml::from_str::<SshV2rayProperty>(content).unwrap()
    });

    static LOCAL_PROP: Lazy<LocalV2rayProperty> = Lazy::new(|| LocalV2rayProperty {
        bin_path: find_v2ray_bin_path().unwrap(),
        config_path: Some(
            Path::new("tests/data")
                .join("local-v2-config.json")
                .to_string_lossy()
                .to_string(),
        ),
    });
}

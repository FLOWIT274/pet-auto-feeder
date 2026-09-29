//! eWelink 集成：Unix socket 客户端（ADR 0004 协议：一行指令一行 JSON）。
//! daemon（ewelink/src/daemon.rs）是唯一控制点：LAN 直连优先，跨网段设备自动云端回退，
//! pulse 一律走设备固件原生 pulse（通电 ms 毫秒后设备自动断电）。
//! socket 协议：ping | quit | on <id> [outlet] | off <id> [outlet] | pulse <id> <ms> [outlet] | status <id>

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::config::Config;

#[derive(Debug)]
pub enum DaemonError {
    Down,       // socket 不存在
    Timeout,    // 响应超时
    Io(String),
}

/// 向 daemon 发一行指令，读一行 JSON 响应
pub async fn dispatch(cfg: &Config, cmd: &str, timeout: Duration) -> Result<Value, DaemonError> {
    let path = cfg.ewelink_socket();
    let mut stream = match UnixStream::connect(&path).await {
        Ok(s) => s,
        Err(_) => return Err(DaemonError::Down),
    };
    if let Err(e) = stream.write_all(cmd.as_bytes()).await {
        return Err(DaemonError::Io(e.to_string()));
    }
    if let Err(e) = stream.write_all(b"\n").await {
        return Err(DaemonError::Io(e.to_string()));
    }
    stream.flush().await.map_err(|e| DaemonError::Io(e.to_string()))?;

    let result = tokio::time::timeout(timeout, async {
        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        Ok::<String, std::io::Error>(line.trim().to_string())
    })
    .await;

    match result {
        Err(_) => Err(DaemonError::Timeout),
        Ok(Err(e)) => Err(DaemonError::Io(e.to_string())),
        Ok(Ok(line)) if line.is_empty() => Err(DaemonError::Io("空响应".into())),
        Ok(Ok(line)) => {
            serde_json::from_str(&line).map_err(|e| DaemonError::Io(format!("JSON 解析失败: {e} | {line}")))
        }
    }
}

/// 设备列表来源 = devicekey 缓存（deviceid → devicekey，ewelink-rs login/list 拉取）
pub fn cached_deviceids(cfg: &Config) -> Vec<String> {
    let path = cfg.ewelink_keys();
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<std::collections::HashMap<String, String>>(&s).ok())
        .map(|m| {
            let mut v: Vec<String> = m.keys().cloned().collect();
            v.sort();
            v
        })
        .unwrap_or_default()
}

pub fn daemon_running(cfg: &Config) -> bool {
    Path::new(&cfg.ewelink_socket()).exists()
}

/// 自愈：socket 不存在时用 ewelink-rs 拉起 daemon（注入 /etc/ewelink.env 凭证），
/// 等 socket 就绪（≤3s）。返回是否可用。
pub async fn ensure_daemon(cfg: &Config) -> bool {
    if daemon_running(cfg) {
        return true;
    }
    if !cfg.ewelink_bin.exists() {
        return false;
    }
    let mut cmd = tokio::process::Command::new(&cfg.ewelink_bin);
    cmd.arg("daemon");
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    if let Ok(raw) = std::fs::read_to_string(&cfg.ewelink_env) {
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                cmd.env(k.trim(), v.trim());
            }
        }
    }
    if cmd.spawn().is_err() {
        return false;
    }
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if daemon_running(cfg) {
            return true;
        }
    }
    false
}

#[derive(Deserialize)]
pub struct PowerReq {
    pub deviceid: String,
    pub outlet: Option<u8>,
}

#[derive(Deserialize)]
pub struct PulseReq {
    pub deviceid: String,
    pub ms: u64,
    pub outlet: Option<u8>,
}

const CMD_TIMEOUT: Duration = Duration::from_secs(15);

fn json_err(msg: &str) -> Value {
    json!({"ok": false, "error": msg})
}

/// 统一走 daemon（唯一控制点）；socket 不在时自愈拉起后重试一次
async fn via_daemon(cfg: &Config, cmd: &str, timeout: Duration) -> Value {
    match dispatch(cfg, cmd, timeout).await {
        Ok(mut v) => {
            v["mode"] = json!("daemon");
            v
        }
        Err(e) => {
            if ensure_daemon(cfg).await {
                match dispatch(cfg, cmd, timeout).await {
                    Ok(mut v) => {
                        v["mode"] = json!("daemon");
                        v
                    }
                    Err(e2) => {
                        let mut v = json_err(&daemon_err_str(e2));
                        v["daemon_running"] = json!(false);
                        v
                    }
                }
            } else {
                let mut v = json_err(&daemon_err_str(e));
                v["daemon_running"] = json!(daemon_running(cfg));
                v
            }
        }
    }
}

/// POST /api/ewelink/{on|off}  body {"deviceid","outlet"?}
pub async fn power(cfg: &Config, action: &str, req: PowerReq) -> Value {
    if req.deviceid.is_empty() {
        return json_err("缺少 deviceid");
    }
    let mut cmd = format!("{action} {}", req.deviceid);
    if let Some(o) = req.outlet {
        cmd.push_str(&format!(" {o}"));
    }
    via_daemon(cfg, &cmd, CMD_TIMEOUT).await
}

/// POST /api/ewelink/pulse  body {"deviceid","ms","outlet"?}
/// daemon 内固件原生 pulse：配置 pulses + 开指令，设备自动断电后立即返回
pub async fn pulse(cfg: &Config, req: PulseReq) -> Value {
    if req.deviceid.is_empty() {
        return json_err("缺少 deviceid");
    }
    if req.ms == 0 || req.ms > 3_600_000 {
        return json_err("ms 需在 1~3600000 (1h) 之间");
    }
    let mut cmd = format!("pulse {} {}", req.deviceid, req.ms);
    if let Some(o) = req.outlet {
        cmd.push_str(&format!(" {o}"));
    }
    via_daemon(cfg, &cmd, CMD_TIMEOUT).await
}

/// POST /api/ewelink/status  body {"deviceid"}
pub async fn status(cfg: &Config, req: PowerReq) -> Value {
    if req.deviceid.is_empty() {
        return json_err("缺少 deviceid");
    }
    via_daemon(cfg, &format!("status {}", req.deviceid), CMD_TIMEOUT).await
}

/// GET /api/ewelink/devices  → 缓存设备列表 + 逐个状态
/// （daemon 现已支持云端回退，跨网段设备也能查询）
pub async fn devices(cfg: &Config) -> Value {
    let running = daemon_running(cfg);
    let ids = cached_deviceids(cfg);
    let mut items: Vec<Value> = Vec::new();
    if running {
        for id in &ids {
            let v = dispatch(cfg, &format!("status {id}"), CMD_TIMEOUT).await;
            match v {
                Ok(v) => items.push(json!({"deviceid": id, "status": v})),
                Err(e) => items.push(json!({"deviceid": id, "error": daemon_err_str(e)})),
            }
        }
    }
    json!({
        "daemon_running": running,
        "deviceids": ids,
        "devices": items,
        "note": if !running { Some("ewelink daemon 未运行") } else { None },
    })
}

/// GET /api/ewelink/health
pub async fn health(cfg: &Config) -> Value {
    json!({
        "daemon_running": daemon_running(cfg),
        "socket": cfg.ewelink_socket().to_string_lossy(),
        "devicekey_cache": cfg.ewelink_keys().to_string_lossy(),
        "cached_devices": cached_deviceids(cfg).len(),
        "cloud": {
            "bin": cfg.ewelink_bin.exists(),
            "env": cfg.ewelink_env.exists(),
        },
    })
}

/// POST /api/ewelink/daemon_start — 用 S90ewelink 拉起 daemon
pub async fn daemon_start(cfg: &Config) -> Value {
    if daemon_running(cfg) {
        return json!({"ok": true, "already_running": true});
    }
    if !cfg.ewelink_init.exists() {
        return json_err(&format!("自启脚本不存在: {}", cfg.ewelink_init.display()));
    }
    let out = tokio::process::Command::new(&cfg.ewelink_init)
        .arg("start")
        .output()
        .await;
    match out {
        Ok(o) => json!({
            "ok": daemon_running(cfg),
            "exit": o.status.code(),
            "stdout": String::from_utf8_lossy(&o.stdout).trim().to_string(),
            "stderr": String::from_utf8_lossy(&o.stderr).trim().to_string(),
        }),
        Err(e) => json_err(&format!("启动失败: {e}")),
    }
}

fn daemon_err_str(e: DaemonError) -> String {
    match e {
        DaemonError::Down => "daemon 未运行（socket 不存在）".into(),
        DaemonError::Timeout => "daemon 响应超时".into(),
        DaemonError::Io(s) => format!("daemon 错误: {s}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    fn cfg_tmp(dir: &tempfile::TempDir) -> Config {
        let mut c = Config::from_env();
        c.home = dir.path().to_path_buf();
        c.ewelink_bin = dir.path().join("ewelink-rs");
        c.ewelink_env = dir.path().join("ewelink.env");
        std::fs::write(&c.ewelink_env, "EWELINK_APPID=fake\nEWELINK_ACCOUNT=u\n").unwrap();
        c
    }

    /// 起一个假 daemon：读一行指令，回一行 JSON
    async fn mock_daemon(cfg: &Config) -> tokio::task::JoinHandle<()> {
        let path = cfg.ewelink_socket();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 256];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).trim().to_string();
            let resp = format!("{{\"ok\":true,\"echo\":\"{req}\"}}\n");
            s.write_all(resp.as_bytes()).await.unwrap();
        })
    }

    #[tokio::test]
    async fn dispatch_roundtrip_through_socket() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_tmp(&dir);
        let h = mock_daemon(&cfg).await;
        let v = dispatch(&cfg, "pulse a1b2c3d4e5 5000 1", CMD_TIMEOUT).await.unwrap();
        h.await.unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["echo"], "pulse a1b2c3d4e5 5000 1");
    }

    #[tokio::test]
    async fn power_pulse_status_all_route_to_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_tmp(&dir);
        let h = mock_daemon(&cfg).await;
        let v = power(&cfg, "on", PowerReq { deviceid: "a1b2c3d4e5".into(), outlet: Some(1) }).await;
        h.await.unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["mode"], "daemon");
        assert_eq!(v["echo"], "on a1b2c3d4e5 1");

        let h = mock_daemon(&cfg).await;
        let v = pulse(&cfg, PulseReq { deviceid: "a1b2c3d4e5".into(), ms: 5000, outlet: Some(1) }).await;
        h.await.unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["echo"], "pulse a1b2c3d4e5 5000 1");

        let h = mock_daemon(&cfg).await;
        let v = status(&cfg, PowerReq { deviceid: "a1b2c3d4e5".into(), outlet: None }).await;
        h.await.unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["echo"], "status a1b2c3d4e5");
    }

    #[tokio::test]
    async fn daemon_down_returns_error_with_running_false() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_tmp(&dir);
        // 无 socket、无 bin → 直接报错，不自愈
        let v = pulse(&cfg, PulseReq { deviceid: "a1b2c3d4e5".into(), ms: 5000, outlet: None }).await;
        assert_eq!(v["ok"], false);
        assert_eq!(v["daemon_running"], false);
        let msg = v["error"].as_str().unwrap_or("");
        assert!(msg.contains("daemon"), "{msg}");
    }

    #[tokio::test]
    async fn ensure_daemon_spawns_bin_when_socket_missing() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("spawn.log");
        let bin = dir.path().join("ewelink-rs");
        std::fs::write(&bin, format!("#!/bin/sh\necho \"$*\" >> \"{}\"\n", log.display())).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = cfg_tmp(&dir);
        // fake bin 只写日志不建 socket → ensure 应 spawn 并超时返回 false
        let ok = ensure_daemon(&cfg).await;
        assert_eq!(ok, false);
        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(calls.trim(), "daemon");
    }
}

//! 常驻局域网守护进程：
//! - 进程内持有 mDNS 后台线程与设备缓存（连续指令间不丢失热状态），消除重复解析延迟；
//! - CLI 薄壳经 Unix socket 每次连接发一行指令、收一行 JSON；
//! - 自动拉起：`lan on/off/status` 发现守护不在时自行 spawn，之后命令命中热缓存。


use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::{api, lan};

const HEADER: &str = "eWeLink-rs daemon/1.0";

fn socket_path() -> PathBuf {
    let mut p = dirs_home();
    p.push(".ewelink-rs-daemon.sock");
    p
}

fn log_path() -> PathBuf {
    let mut p = dirs_home();
    p.push(".ewelink-rs-daemon.log");
    p
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

static QUIT: AtomicBool = AtomicBool::new(false);

/// 守护进程是否在监听
pub fn daemon_running() -> bool {
    std::os::unix::net::UnixStream::connect(socket_path()).is_ok()
}

/// 薄壳同步请求：连 socket → 发一行指令 → 读一行 JSON 响应（响应超时由调用方指定）。
pub fn dispatch(cmd: &str, resp_timeout: Duration) -> Option<String> {
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(socket_path()).ok()?;
    s.set_read_timeout(Some(resp_timeout)).ok()?;
    s.write_all(cmd.as_bytes()).ok()?;
    s.write_all(b"\n").ok()?;
    s.flush().ok()?;
    let mut resp = String::new();
    s.read_to_string(&mut resp).ok()?;
    let line = resp.lines().next().unwrap_or("").to_string();
    (!line.is_empty()).then_some(line)
}

/// 保证守护进程在监听；不在则自动 spawn（stdout/stderr 丢弃，日志写文件）。
/// 返回是否可用。
pub fn ensure_daemon() -> bool {
    if daemon_running() {
        return true;
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return false,
    };
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
        .ok();
    let stderr = stdout.as_ref().and_then(|f| f.try_clone().ok());
    let _ = Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(match stdout {
            Some(f) => Stdio::from(f),
            None => Stdio::null(),
        })
        .stderr(match stderr {
            Some(f) => Stdio::from(f),
            None => Stdio::null(),
        })
        .spawn();
    // 等 socket 就绪（≤3s）
    for _ in 0..30 {
        if daemon_running() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// daemon 主入口：前台常驻，直到收到 quit。
pub async fn run_daemon() -> Result<()> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            bail!("无法绑定守护 socket（已有实例在运行？）: {e:#}");
        }
    };
    // 预热局域网状态（启动后台 mDNS 监听线程）
    lan::LanCache::warm();
    logln(format!("daemon 启动 socket={path:?}"));
    while !QUIT.load(Ordering::Relaxed) {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(handle(stream));
            }
            Err(e) => {
                logln(format!("accept 错误: {e:#}"));
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
    let _ = std::fs::remove_file(&path);
    logln("daemon 退出".into());
    Ok(())
}

async fn handle(mut stream: UnixStream) {
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    if reader.read_line(&mut line).await.is_err() {
        return;
    }
    let req = line.trim().to_string();
    let (resp, quit) = process(&req).await;
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.flush().await;
    if quit {
        QUIT.store(true, Ordering::Relaxed);
    }
}

/// 常驻云端客户端（token 缓存：首次登录后 at/rt 长期复用，到期自动 refresh，
/// 避免每次控制都走完整 OAuth 授权，省 API 配额）
static CLOUD: std::sync::OnceLock<tokio::sync::Mutex<Option<api::Client>>> =
    std::sync::OnceLock::new();

fn cloud_mutex() -> &'static tokio::sync::Mutex<Option<api::Client>> {
    CLOUD.get_or_init(|| tokio::sync::Mutex::new(None))
}

/// 取云端客户端：首次按 /etc/ewelink.env 凭证构造，之后复用已登录实例
async fn cloud_client() -> Result<tokio::sync::MutexGuard<'static, Option<api::Client>>> {
    let mut guard = cloud_mutex().lock().await;
    if guard.is_none() {
        match api::Client::from_env() {
            Ok(c) => *guard = Some(c),
            Err(e) => bail!("云端凭证缺失（需 /etc/ewelink.env）: {e:#}"),
        }
    }
    if let Some(c) = guard.as_mut() {
        c.ensure_auth().await?;
    }
    Ok(guard)
}

/// 云端调用 + 401 自愈：token 被并发登录顶掉/失效时，重新登录后重试一次。
/// $call 必须是直接表达式（如 client.set_outlet(...)），每次失败后重新求值。
macro_rules! cloud_retry {
    ($client:expr, $call:expr) => {{
        match $call.await {
            Ok(v) => Ok(v),
            Err(e) if format!("{e}").contains("401") => {
                eprintln!("云端 401（token 被顶掉/失效）→ 重新登录后重试");
                $client.login().await?;
                $call.await
            }
            Err(e) => Err(e),
        }
    }};
}

/// 云端固件 pulse（401 自愈）。outlet=None → 全部插位
/// （物理设备可能仅一个插座但固件虚报多通道，全插位确保任何通道都被触发）
/// 中间态优化：通道数首次查询后缓存，pulses+switches 组合包一次下发，失败回退两步法
async fn cloud_pulse_op(id: &str, outlet: Option<u8>, ms: u64) -> Result<()> {
    let mut guard = cloud_client().await?;
    let client = guard.as_mut().expect("cloud_client 保证 Some");
    match outlet {
        Some(o) => {
            cloud_retry!(client, client.set_pulse(id, o, Some(ms)))?;
            cloud_retry!(client, client.set_outlet(id, o, true))?;
        }
        None => {
            let n = channels(client, id).await?;
            match cloud_retry!(client, client.pulse_all_combined(id, ms, n)) {
                Ok(_) => {}
                Err(_) => {
                    cloud_retry!(client, client.set_pulse_all(id, ms, n))?;
                    cloud_retry!(client, client.set_switch(id, true))?;
                }
            }
        }
    }
    Ok(())
}

/// 设备通道数缓存：首次 get_status 获取后复用（全插位 pulse 免每次 status API）。
static CHANNELS: std::sync::OnceLock<tokio::sync::Mutex<Option<u8>>> =
    std::sync::OnceLock::new();

async fn channels(client: &mut api::Client, id: &str) -> Result<u8> {
    let mut g = CHANNELS
        .get_or_init(|| tokio::sync::Mutex::new(None))
        .lock()
        .await;
    if let Some(n) = *g {
        return Ok(n);
    }
    let params = cloud_retry!(client, client.get_status(id))?;
    let n = params
        .get("pulses")
        .and_then(|p| p.as_array())
        .map(|a| a.len())
        .or_else(|| {
            params
                .get("switches")
                .and_then(|s| s.as_array())
                .map(|a| a.len())
        })
        .unwrap_or(4) as u8;
    *g = Some(n);
    Ok(n)
}

/// 云端状态查询（401 自愈）
async fn cloud_status_op(id: &str) -> Result<Value> {
    let mut guard = cloud_client().await?;
    let client = guard.as_mut().expect("cloud_client 保证 Some");
    cloud_retry!(client, client.get_status(id))
}

/// 云端开/关（401 自愈）
async fn cloud_power_op(id: &str, on: bool, outlet: Option<u8>) -> Result<Value> {
    let mut guard = cloud_client().await?;
    let client = guard.as_mut().expect("cloud_client 保证 Some");
    match outlet {
        Some(o) => cloud_retry!(client, client.set_outlet(id, o, on)),
        None => cloud_retry!(client, client.set_switch(id, on)),
    }
}

async fn process(req: &str) -> (String, bool) {
    let mut it = req.split_whitespace();
    let action = it.next().unwrap_or("");
    let id = it.next().unwrap_or("");
    let mut quit = false;
    let v: Value = match action {
        "ping" | "hello" => json!({"ok": true, "mode": HEADER, "version": env!("CARGO_PKG_VERSION")}),
        "quit" => {
            quit = true;
            json!({"ok": true})
        }
        "on" | "off" => {
            if id.is_empty() {
                json!({"ok": false, "error": "缺少 deviceid"})
            } else {
                let outlet = it.next().and_then(|s| s.parse::<u8>().ok());
                set_power(id, action == "on", outlet).await
            }
        }
        "pulse" => {
            let ms = it.next().unwrap_or("").parse::<u64>().unwrap_or(0);
            let outlet = it.next().and_then(|s| s.parse::<u8>().ok());
            pulse(id, ms, outlet).await
        }
        "status" => status(id).await,
        _ => json!({"ok": false, "error": format!("未知指令: {action}")}),
    };
    let mut s = serde_json::to_string(&v).unwrap_or_else(|_| r#"{"ok":false}"#.into());
    s.push('\n');
    (s, quit)
}

/// 固件原生 pulse（一体化）：配置设备自带的 pulses 参数 + 发送开指令，
/// 通电 `ms` 毫秒后**设备固件自动断电**（LAN 与云端路径统一，无需宿主计时）。
fn check_pulse_args(id: &str, ms: u64) -> Result<()> {
    const MAX: u64 = 3_600_000;
    if ms == 0 || ms > MAX {
        bail!("时长需在 1~{MAX}ms (1h) 之间，实际收到 {ms}ms");
    }
    if id.is_empty() {
        bail!("缺少 deviceid");
    }
    Ok(())
}

async fn pulse(id: &str, ms: u64, outlet: Option<u8>) -> Value {
    if let Err(e) = check_pulse_args(id, ms) {
        return json!({"ok": false, "error": format!("{e:#}")});
    }
    let scope = outlet
        .map(|o| format!("插位 {o}"))
        .unwrap_or_else(|| "全部插位".to_string());
    // 优先局域网（设备同网段且密钥已缓存）→ LAN 下发固件 pulses 参数
    if let (Ok(key), Ok((dev, _))) = (
        load_key(id).await,
        lan::LanCache::get(id, Duration::from_millis(500)),
    ) {
        let r = match outlet {
            Some(o) => match lan::set_pulse(&dev, &key, o, Some(ms)).await {
                Ok(_) => lan::set_switches(&dev, &key, o, true).await,
                Err(e) => Err(e),
            },
            None => match lan::set_pulse_all(&dev, &key, ms).await {
                Ok(_) => lan::set_all_switches(&dev, &key, true).await,
                Err(e) => Err(e),
            },
        };
        return match r {
            Ok(_) => json!({
                "ok": true, "mode": "lan", "native": true, "target_ms": ms, "outlet": outlet,
                "msg": format!("固件原生 pulse(LAN) 已触发：{scope}通电 {ms}ms 后自动断电")
            }),
            Err(e) => json!({"ok": false, "mode": "lan", "error": format!("LAN 触发失败: {e:#}")}),
        };
    }
    // 云端回退（跨网段设备）
    match cloud_pulse_op(id, outlet, ms).await {
        Ok(_) => json!({
            "ok": true, "mode": "cloud", "native": true, "target_ms": ms, "outlet": outlet,
            "msg": format!("固件原生 pulse(云) 已触发：{scope}通电 {ms}ms 后自动断电")
        }),
        Err(e) => json!({"ok": false, "mode": "cloud", "error": format!("云端触发失败: {e:#}")}),
    }
}

/// 查询状态：LAN 快照优先，跨网段设备走云端
async fn status(id: &str) -> Value {
    if id.is_empty() {
        return json!({"ok": false, "error": "缺少 deviceid"});
    }
    if let (Ok(key), Ok((dev, _))) = (
        load_key(id).await,
        lan::LanCache::get(id, Duration::from_millis(500)),
    ) {
        match lan::decrypt_snapshot(&dev.props, &key) {
            Ok(snap) => {
                return json!({
                    "ok": true, "mode": "lan", "deviceid": id, "addr": dev.host_port(),
                    "name": dev.name, "snapshot": snap,
                })
            }
            Err(e) => {
                return json!({"ok": false, "mode": "lan", "error": format!("解密快照失败: {e:#}")})
            }
        }
    }
    match cloud_status_op(id).await {
        Ok(params) => json!({"ok": true, "mode": "cloud", "deviceid": id, "snapshot": params}),
        Err(e) => json!({"ok": false, "mode": "cloud", "error": format!("云端查询失败: {e:#}")}),
    }
}

/// 开/关：LAN 命中走本地直连（原逻辑），跨网段设备云端回退
async fn set_power(id: &str, on: bool, outlet: Option<u8>) -> Value {
    if let (Ok(key), Ok((dev, ts))) = (
        load_key(id).await,
        lan::LanCache::get(id, Duration::from_millis(500)),
    ) {
        return lan_set_power(&dev, &key, id, on, outlet, ts).await;
    }
    // 云端回退
    let action = if on { "开" } else { "关" };
    let scope = outlet
        .map(|o| format!("outlet {o}"))
        .unwrap_or_else(|| "全部插位".to_string());
    match cloud_power_op(id, on, outlet).await {
        Ok(_) => json!({
            "ok": true, "mode": "cloud", "sent": format!("云 {action}({scope}) 已发送到 {id}"),
            "confirm": true, "confirm_msg": format!("✓ 云端已确认(error:0)：{scope}已{action}"),
        }),
        Err(e) => json!({"ok": false, "mode": "cloud", "error": format!("云端控制失败: {e:#}")}),
    }
}

/// 局域网直连开/关（原 daemon 逻辑：下发 + 等快照回读确认）
async fn lan_set_power(
    dev: &lan::LanDevice,
    key: &str,
    id: &str,
    on: bool,
    outlet: Option<u8>,
    before: Instant,
) -> Value {
    let action = if on { "开" } else { "关" };
    let scope = outlet
        .map(|o| format!("outlet {o}"))
        .unwrap_or_else(|| "全部插位".to_string());
    let sent = match outlet {
        Some(o) => {
            if let Err(e) = lan::set_switches(dev, key, o, on).await {
                return json!({"ok": false, "mode": "lan", "error": format!("{e:#}")});
            }
            format!("LAN {action}(outlet={o}) 已发送到 {} ({})", dev.deviceid, dev.host_port())
        }
        None => {
            if let Err(e) = lan::set_all_switches(dev, key, on).await {
                return json!({"ok": false, "mode": "lan", "error": format!("{e:#}")});
            }
            format!("LAN 全部插位{action} 已发送到 {} ({})", dev.deviceid, dev.host_port())
        }
    };
    // 等设备主动广播新快照，逐通道比对确认
    let (ok, snap) = match lan::LanCache::wait_refresh(id, before, Duration::from_secs(3)) {
        Some(dev2) => lan::check_snapshot(&dev2, key, on, outlet),
        None => (false, None),
    };
    let target = match snap.as_ref() {
        Some(v) => v.get("switches").cloned().unwrap_or(Value::Null),
        None => Value::Null,
    };
    let confirm_msg = match (ok, snap) {
        (true, _) => format!("✓ 已回读确认：{scope}已{action}"),
        (false, Some(_)) => format!("⚠ 设备已执行 (error:0)，但回读状态与目标不一致: {target}"),
        (false, None) => "（未能回读确认：mDNS 快照暂不可得；设备已返回 error:0 确认执行）".to_string(),
    };
    json!({"ok": true, "mode": "lan", "sent": sent, "confirm": ok, "confirm_msg": confirm_msg})
}

/// 从本地缓存取 devicekey；缺失时报告需先 login 拉取。
async fn load_key(id: &str) -> Result<String, String> {
    let keys: HashMap<String, String> = lan::load_device_keys();
    match keys.get(id) {
        Some(k) => Ok(k.clone()),
        None => Err(
            "该设备的 devicekey 缺失：请先运行 `ewelink-rs login` 拉取密钥缓存".to_string(),
        ),
    }
}

fn logln(msg: String) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = writeln!(f, "{} {msg}", chrono_like_now());
    }
}

/// 简易时间戳（无 chrono 依赖）
fn chrono_like_now() -> String {
    let s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("t+{}ms", s)
}
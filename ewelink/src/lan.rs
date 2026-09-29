//! eWeLink 局域网 zeroconf 直连控制（协议已对真实设备实测验证）
//!
//! 概览（与云端 API 无关，全部走本地网络，**不受 eWeLink 免费账号 API 限额约束**）：
//!
//! 1. **发现**：mDNS 浏览 `_ewelink._tcp.local.`，获取设备 IP、端口（默认 8081）
//!    以及 TXT 记录：`id`、`type`、`encrypt`、`data1..4`、`iv`、`seq`。
//! 2. **状态**：TXT 里的 `data1..4` + `iv` 是设备加密广播的当前状态快照，
//!    用 devicekey 解密即得到实时状态（switches 数组等），设备每次状态变化都会推送新 `seq`。
//! 3. **控制**：`POST http://<ip>:8081/zeroconf/switches`，body 加密：
//!    ```json
//!    {"sequence":"<毫秒>","deviceid":"<id>","selfApikey":"123",
//!     "encrypt":true,"data":"<base64密文>","iv":"<base64 iv>"}
//!    ```
//!    data 解密后为 `{"switches":[{"switch":"on|off","outlet":n}]}`，响应 `{"error":0}` 即成功。
//!    注意：本固件（uiid=138）只认复数命令名 `switches`；`switch`/`query`/`getState` 均返回 400。
//!
//! **加密**：AES-128-CBC + PKCS7，`key = MD5(devicekey)`，iv 随机 16 字节。
//! devicekey 从云端 `/v2/device/thing` 获取（与设备无关时一般长期不变，本地缓存）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use aes::Aes128;
use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use cbc::{Decryptor, Encryptor};
use cipher::block_padding::{NoPadding, Pkcs7};
use cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use md5::{Digest, Md5};
use serde_json::{json, Value};

type AesCbcEnc = Encryptor<Aes128>;
type AesCbcDec = Decryptor<Aes128>;

/// 一台通过 mDNS 发现的 eWeLink 设备
#[derive(Debug, Clone)]
pub struct LanDevice {
    /// 10 位设备 ID（如 a1b2c3d4e5）
    pub deviceid: String,
    /// mDNS 服务名（如 eWeLink_a1b2c3d4e5）
    pub name: String,
    pub addr: String,
    pub port: u16,
    /// mDNS TXT 全部属性（含 data1..4 / iv / seq / type，即最新状态快照）
    pub props: HashMap<String, String>,
}

impl LanDevice {
    pub fn host_port(&self) -> String {
        format!("{}:{}", self.addr, self.port)
    }
}

/// mDNS 扫描 `_ewelink._tcp.local.`，持续 `timeout` 收集（同设备多次广播取最新）。
/// 返回设备列表；期间收到的每次 ServiceResolved 都会刷新对应设备。
/// 自适应收尾：一旦发现过设备且 1.5s 内没有新设备出现，提前结束（快速网络秒回），
/// 整体仍受 timeout 上限保护（慢速网络/多设备场景不遗漏）。
pub fn discover(timeout: Duration) -> Result<Vec<LanDevice>> {
    let daemon = mdns_sd::ServiceDaemon::new().context("初始化 mDNS 失败")?;
    let receiver = daemon
        .browse("_ewelink._tcp.local.")
        .context("mDNS 浏览 _ewelink._tcp.local. 失败")?;

    let mut map: HashMap<String, LanDevice> = HashMap::new();
    let deadline = std::time::Instant::now() + timeout;
    let mut last_new = std::time::Instant::now();
    while std::time::Instant::now() < deadline {
        match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                if let Some(dev) = extract_device(&info) {
                    if map.insert(dev.deviceid.clone(), dev).is_none() {
                        last_new = std::time::Instant::now();
                    }
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
        // 自适应：已发现设备且 1.5s 无新增 → 提前收尾
        if !map.is_empty() && last_new.elapsed() >= Duration::from_millis(1500) {
            break;
        }
    }
    daemon.shutdown()?;
    let mut out: Vec<LanDevice> = map.into_values().collect();
    out.sort_by(|a, b| a.deviceid.cmp(&b.deviceid));
    Ok(out)
}

// ---------- 常驻 mDNS 守护 + 设备缓存（5 分钟有效） ----------

/// 后台线程持续监听 mDNS 主动广播，维护最新设备快照缓存。
/// 控制路径命中缓存时无需再等待主动查询，大幅缩短局域网操作延迟。
pub struct LanCache {
    /// 保持 daemon 存活（drop 会使浏览会话失效）
    #[allow(dead_code)]
    daemon: mdns_sd::ServiceDaemon,
    entries: Mutex<HashMap<String, CacheEntry>>,
    /// 负缓存：resolve 失败的设备（不在本网段）短时间内不再重复查询
    misses: Mutex<HashMap<String, Instant>>,
    cond: Condvar,
}

struct CacheEntry {
    dev: LanDevice,
    ts: Instant,
}

static CACHE: OnceLock<Arc<LanCache>> = OnceLock::new();

impl LanCache {
    /// 启动（或复用）全局守护实例；用于预热后台 mDNS 监听线程。
    pub fn warm() {
        Self::global();
    }

    fn global() -> &'static Arc<LanCache> {
        CACHE.get_or_init(|| {
            let daemon = mdns_sd::ServiceDaemon::new().expect("初始化 mDNS 失败");
            let rx = daemon
                .browse("_ewelink._tcp.local.")
                .expect("mDNS 浏览失败");
            let cache = Arc::new(LanCache {
                daemon,
                entries: Mutex::new(HashMap::new()),
                misses: Mutex::new(HashMap::new()),
                cond: Condvar::new(),
            });
            let pump = cache.clone();
            std::thread::spawn(move || {
                // 事件泵：持续收广播，更新缓存并唤醒等待者
                loop {
                    match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                            if let Some(dev) = extract_device(&info) {
                                if let Ok(mut m) = pump.entries.lock() {
                                    m.insert(
                                        dev.deviceid.clone(),
                                        CacheEntry {
                                            dev: dev.clone(),
                                            ts: Instant::now(),
                                        },
                                    );
                                    drop(m);
                                    // 设备广播到达 = 已上线同网段 → 立即清除负缓存，
                                    // 避免 60s 窗口内 LAN 路径仍被上次的 miss 挡住
                                    if let Ok(mut mm) = pump.misses.lock() {
                                        mm.remove(&dev.deviceid);
                                    }
                                    pump.cond.notify_all();
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(_) => {
                            // 超时（或会话断开，极少见）：继续轮询，避免忙转
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    }
                }
            });
            cache
        })
    }

    /// 缓存条目有效期：5 分钟（5 min）。
    const TTL: Duration = Duration::from_secs(300);
    /// 负缓存有效期：1 分钟（resolve 失败后短时间直接判 LAN 不可达，
    /// 避免跨网段设备每次控制都白等 resolve 超时）。
    const MISS_TTL: Duration = Duration::from_secs(60);

    /// 取设备：缓存命中且未过期直接返回（毫秒级）；负缓存内直接失败；
    /// 否则走完整 resolve，失败记入负缓存。
    /// 返回 (设备, 该条目的时间戳)。
    pub fn get(devid: &str, timeout: Duration) -> Result<(LanDevice, Instant)> {
        let cache = Self::global();
        {
            let m = cache.entries.lock().unwrap();
            if let Some(e) = m.get(devid) {
                if e.ts.elapsed() < Self::TTL {
                    return Ok((e.dev.clone(), e.ts));
                }
            }
        }
        {
            let m = cache.misses.lock().unwrap();
            if let Some(ts) = m.get(devid) {
                if ts.elapsed() < Self::MISS_TTL {
                    return Err(anyhow!(
                        "设备 {devid} LAN 不可达（负缓存，60s 内不重复查询）"
                    ));
                }
            }
        }
        match resolve_one_retry(devid, timeout) {
            Ok(dev) => {
                let ts = Instant::now();
                cache
                    .entries
                    .lock()
                    .unwrap()
                    .insert(devid.to_string(), CacheEntry { dev: dev.clone(), ts });
                Ok((dev, ts))
            }
            Err(e) => {
                cache
                    .misses
                    .lock()
                    .unwrap()
                    .insert(devid.to_string(), Instant::now());
                Err(e)
            }
        }
    }

    /// 等待该设备在 `before` 之后出现新广播快照（进程内 <1s 通常可达），
    /// 超时返回 None（不误报失败，调用方降级处理）。
    pub fn wait_refresh(devid: &str, before: Instant, wait: Duration) -> Option<LanDevice> {
        let cache = Self::global();
        let deadline = Instant::now() + wait;
        loop {
            {
                let m = cache.entries.lock().unwrap();
                if let Some(e) = m.get(devid) {
                    if e.ts > before {
                        return Some(e.dev.clone());
                    }
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            let _ = cache
                .cond
                .wait_timeout(cache.entries.lock().unwrap(), Duration::from_millis(200));
        }
    }
}

/// 从 ServiceResolved 提取 LanDevice（discover/resolve/守护线程共用）。
fn extract_device(info: &mdns_sd::ServiceInfo) -> Option<LanDevice> {
    let props: HashMap<String, String> = info
        .get_properties()
        .iter()
        .map(|p| (p.key().to_string(), p.val_str().to_string()))
        .collect();
    let deviceid = props
        .get("id")
        .cloned()
        .or_else(|| {
            let n = info.get_fullname();
            (n.len() >= 18).then(|| n[8..18].to_string())
        })
        .unwrap_or_default();
    if deviceid.is_empty() {
        return None;
    }
    let addr = info
        .get_addresses()
        .iter()
        .find_map(|a| match a {
            std::net::IpAddr::V4(v4) => Some(v4.to_string()),
            _ => None,
        })
        .unwrap_or_default();
    if addr.is_empty() {
        return None;
    }
    Some(LanDevice {
        deviceid,
        name: info.get_fullname().to_string(),
        addr,
        port: info.get_port(),
        props,
    })
}

// ---------- 常驻缓存（end） ----------

/// 重新 mDNS 解析单个设备：返回最新快照（设备状态变化会广播新 seq）。
/// 设备对相同查询有 ~1 秒响应抑制（RFC 6762），故失败后自动间隔重试。
pub fn resolve_one_retry(devid: &str, timeout: Duration) -> Result<LanDevice> {
    let mut last_err: Option<anyhow::Error> = None;
    for i in 0..3 {
        match resolve_one(devid, timeout) {
            Ok(d) => return Ok(d),
            Err(e) => {
                last_err = Some(e);
                if i < 2 {
                    std::thread::sleep(Duration::from_millis(1500));
                }
            }
        }
    }
    Err(last_err.unwrap())
}

fn resolve_one(devid: &str, timeout: Duration) -> Result<LanDevice> {
    let daemon = mdns_sd::ServiceDaemon::new().context("初始化 mDNS 失败")?;
    let receiver = daemon
        .browse("_ewelink._tcp.local.")
        .context("mDNS 浏览失败")?;
    let deadline = std::time::Instant::now() + timeout;
    let mut latest: Option<LanDevice> = None;
    while std::time::Instant::now() < deadline {
        match receiver.recv_timeout(Duration::from_millis(300)) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                let is_target = info.get_fullname().starts_with(&format!("eWeLink_{devid}."))
                    || info.get_fullname().starts_with(&format!("eWelink_{devid}."))
                    || info
                        .get_properties()
                        .iter()
                        .any(|p| p.key() == "id" && p.val_str() == devid);
                if !is_target {
                    continue;
                }
                if let Some(mut dev) = extract_device(&info) {
                    // 保留上一次已知地址，防止 TXT 响应偶发缺地址
                    if dev.addr.is_empty() {
                        if let Some(prev) = &latest {
                            dev.addr = prev.addr.clone();
                        }
                    }
                    dev.deviceid = devid.to_string();
                    latest = Some(dev);
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    daemon.shutdown()?;
    latest.context(format!("未在局域网发现设备 {devid}"))
}

/// 解密 mDNS TXT 状态快照 → 设备参数 JSON
pub fn decrypt_snapshot(props: &HashMap<String, String>, devicekey: &str) -> Result<Value> {
    let raw: String = (1..=4)
        .filter_map(|i| props.get(&format!("data{i}")))
        .cloned()
        .collect();
    if raw.is_empty() {
        bail!("设备 TXT 无数据快照（不支持局域网状态，请用云 status）");
    }
    let iv = props.get("iv").context("设备 TXT 缺少 iv")?;
    let key = aes_key(devicekey);
    let pt = aes_cbc_decrypt(&raw, iv, &key)?;
    serde_json::from_slice(&pt).context("快照 JSON 解析失败")
}

/// 局域网控制：单通道开关指令（uiid=138 4 通道插座，命令必须是复数 `switches`）
///
/// 手写 HTTP/1.1：设备固件（openresty + lua 手写解析器）对 Header 名大小写敏感，
/// 只认大写（Host/Content-Type/Connection 等）；reqwest/hyper 强制输出小写头名会被
/// 设备直接关闭连接（实测 `connection closed before message completed`）。
pub async fn set_switches(
    dev: &LanDevice,
    devicekey: &str,
    outlet: u8,
    on: bool,
) -> Result<Value> {
    let data = json!({
        "switches": [{ "outlet": outlet, "switch": if on { "on" } else { "off" } }]
    });
    set_switches_payload(dev, devicekey, &data).await
}

/// 局域网控制：全部插位同时开/关。
/// 通道数优先取设备推送快照中的 `switches` 长度，拿不到快照时保守按 4 通道处理。
pub async fn set_all_switches(dev: &LanDevice, devicekey: &str, on: bool) -> Result<Value> {
    let n = decrypt_snapshot(&dev.props, devicekey)
        .ok()
        .and_then(|v| {
            v.get("switches")
                .and_then(|s| s.as_array())
                .map(|a| a.len())
        })
        .unwrap_or(4);
    let mut list = Vec::with_capacity(n);
    for outlet in 0..n as u8 {
        list.push(json!({ "outlet": outlet, "switch": if on { "on" } else { "off" } }));
    }
    let data = json!({ "switches": list });
    set_switches_payload(dev, devicekey, &data).await
}

/// 局域网固件原生 pulse：配置插位 pulse（width 毫秒后设备自动断电）并保持其余插位原配置。
/// ms=Some(w) 启用，None 关闭。通道数取快照 pulses/switches 长度，保守 4。
pub async fn set_pulse(
    dev: &LanDevice,
    devicekey: &str,
    outlet: u8,
    ms: Option<u64>,
) -> Result<Value> {
    let n = decrypt_snapshot(&dev.props, devicekey)
        .ok()
        .and_then(|v| {
            v.get("pulses")
                .or_else(|| v.get("switches"))
                .and_then(|s| s.as_array())
                .map(|a| a.len())
        })
        .unwrap_or(4);
    let mut list: Vec<Value> = (0..n)
        .map(|i| {
            if i as u8 == outlet {
                match ms {
                    Some(w) => json!({ "outlet": i, "pulse": "on", "switch": "off", "width": w }),
                    None => json!({ "outlet": i, "pulse": "off", "switch": "off", "width": 0 }),
                }
            } else {
                json!({ "outlet": i, "pulse": "off", "switch": "off", "width": 0 })
            }
        })
        .collect();
    // 保留快照里其它插位已配置的 pulse（避免覆盖用户在 App 里设的定时）
    if let Ok(snap) = decrypt_snapshot(&dev.props, devicekey) {
        if let Some(ps) = snap.get("pulses").and_then(|p| p.as_array()) {
            for (i, p) in ps.iter().enumerate() {
                if i == outlet as usize || i >= list.len() {
                    continue;
                }
                list[i] = p.clone();
            }
        }
    }
    let params = json!({ "pulses": list });
    set_switches_payload(dev, devicekey, &params).await
}

/// 局域网全部插位固件原生 pulse：所有通道 width 毫秒后自动断电。
/// 物理设备可能仅一个插座但固件虚报多通道——全插位操作确保任何通道都被触发。
pub async fn set_pulse_all(dev: &LanDevice, devicekey: &str, ms: u64) -> Result<Value> {
    let n = decrypt_snapshot(&dev.props, devicekey)
        .ok()
        .and_then(|v| {
            v.get("pulses")
                .or_else(|| v.get("switches"))
                .and_then(|s| s.as_array())
                .map(|a| a.len())
        })
        .unwrap_or(4);
    let list: Vec<Value> = (0..n)
        .map(|i| json!({ "outlet": i, "pulse": "on", "switch": "off", "width": ms }))
        .collect();
    let params = json!({ "pulses": list });
    set_switches_payload(dev, devicekey, &params).await
}

async fn set_switches_payload(
    dev: &LanDevice,
    devicekey: &str,
    data: &Value,
) -> Result<Value> {
    let key = aes_key(devicekey);
    let (data_b64, iv_b64) = aes_cbc_encrypt(data, &key)?;
    let body = json!({
        "sequence": format!("{}", now_ms()),
        "deviceid": dev.deviceid,
        "selfApikey": "123",
        "encrypt": true,
        "data": data_b64,
        "iv": iv_b64,
    })
    .to_string();

    let url = format!("http://{}/zeroconf/switches", dev.host_port());
    let request = format!(
        "POST /zeroconf/switches HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/json\r\n\
         Connection: close\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        dev.host_port(),
        body.len(),
        body
    );

    // 设备 web server 单线程：忙碌时会关闭新连接（SonoffLAN 现象），失败后间隔 0.3s 重试 ≤10 次。
    let mut last_err: Option<anyhow::Error> = None;
    for _ in 0..10 {
        match send_raw(&dev.addr, dev.port, &request).await {
            Ok(resp) => {
                let err = resp.get("error").and_then(|e| e.as_i64()).unwrap_or(-1);
                if err != 0 {
                    bail!("设备返回错误: error={err}（{resp}）");
                }
                return Ok(resp);
            }
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
    Err(last_err.unwrap()).with_context(|| format!("局域网请求失败: {url}"))
}

/// 建立 TCP 连接、发送完整 HTTP 请求，读取响应并解析 JSON body。
/// 响应头带 `Connection: close`，读到 EOF 即可取完整个响应。
async fn send_raw(addr: &str, port: u16, request: &str) -> Result<Value> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect((addr, port))
        .await
        .with_context(|| format!("连接 {addr}:{port} 失败"))?;
    stream.write_all(request.as_bytes()).await?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let text = String::from_utf8_lossy(&buf);
    let body_part = text
        .split("\r\n\r\n")
        .nth(1)
        .context("设备响应缺少 body")?;
    Ok(serde_json::from_str(body_part)?)
}

/// 比对设备快照：目标插位状态是否真正达成。
/// 返回 (是否全部达成, 快照内容)。快照不可用时返回 (false, None)。
pub fn check_snapshot(
    dev: &LanDevice,
    devicekey: &str,
    on: bool,
    outlet: Option<u8>,
) -> (bool, Option<Value>) {
    let want = if on { "on" } else { "off" };
    match decrypt_snapshot(&dev.props, devicekey) {
        Ok(v) => {
            let ok = v
                .get("switches")
                .and_then(|s| s.as_array())
                .map(|arr| {
                    !arr.is_empty()
                        && arr.iter().all(|sw| {
                            let o = sw.get("outlet").and_then(|x| x.as_u64());
                            let st = sw.get("switch").and_then(|x| x.as_str());
                            match outlet {
                                Some(target) => o == Some(target as u64) && st == Some(want),
                                None => st == Some(want),
                            }
                        })
                })
                .unwrap_or(false);
            (ok, Some(v))
        }
        Err(_) => (false, None),
    }
}

// ---------- 加密工具 ----------

pub fn aes_key(devicekey: &str) -> [u8; 16] {
    let mut h = Md5::new();
    h.update(devicekey.as_bytes());
    let d = h.finalize();
    let mut k = [0u8; 16];
    k.copy_from_slice(&d);
    k
}

fn aes_cbc_encrypt(data: &Value, key: &[u8; 16]) -> Result<(String, String)> {
    let pt = serde_json::to_vec(data).context("data 序列化失败")?;
    let iv: [u8; 16] = rand::random();
    let c = AesCbcEnc::new_from_slices(key, &iv).expect("AES 密钥/IV 长度 16，必然合法");
    let ct = c.encrypt_padded_vec_mut::<Pkcs7>(&pt);
    Ok((B64.encode(ct), B64.encode(iv)))
}

fn aes_cbc_decrypt(data_b64: &str, iv_b64: &str, key: &[u8; 16]) -> Result<Vec<u8>> {
    let ct = B64.decode(data_b64).context("data base64 解码失败")?;
    let iv = B64.decode(iv_b64).context("iv base64 解码失败")?;
    if iv.len() != 16 {
        bail!("iv 长度 {} ≠ 16", iv.len());
    }
    let c = AesCbcDec::new_from_slices(key, &iv).expect("初始化 密钥/IV 长度 16，必然合法");
    match c.decrypt_padded_vec_mut::<Pkcs7>(&ct) {
        Ok(pt) => Ok(pt),
        Err(_) => {
            // 个别固件尾部带非标准填充（SonoffLAN issue #1160），降级：无填充解密 + 手动去尾
            let raw = AesCbcDec::new_from_slices(key, &iv)
                .expect("初始化密钥/IV 必然合法")
                .decrypt_padded_vec_mut::<NoPadding>(&ct)
                .unwrap_or_default();
            if raw.is_empty() {
                bail!("解密结果为空");
            }
            let pad = raw[raw.len() - 1] as usize;
            let end = if (1..=16).contains(&pad) && pad <= raw.len() {
                raw.len() - pad
            } else {
                raw.len()
            };
            Ok(raw[..end].to_vec())
        }
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// ---------- devicekey 本地缓存 ----------

fn cache_path() -> PathBuf {
    let home = std::env::var("EWL_DEVICEKEY_FILE").ok().map(PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(PathBuf::from)
            .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from)))
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".ewelink-rs-devicekeys.json")
}

/// 读取本地 devicekey 缓存（deviceid → devicekey）
pub fn load_device_keys() -> HashMap<String, String> {
    let path = cache_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_device_keys(map: &HashMap<String, String>) {
    if let Ok(s) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(cache_path(), s);
    }
}

/// 取全部发现设备的 devicekey：缓存命中直接返回（离线可用），缺失的登录云端拉取并写缓存
pub async fn ensure_device_keys(
    client: &mut crate::api::Client,
    deviceids: &[String],
) -> Result<HashMap<String, String>> {
    let mut map = load_device_keys();
    let mut needed: Vec<&String> = deviceids.iter().filter(|d| !map.contains_key(*d)).collect();
    needed.sort();
    needed.dedup();
    if !needed.is_empty() {
        if client.at.is_none() {
            client.login().await?;
        }
        let devices = client.list_devices().await?;
        for d in &devices {
            if let Some(k) = &d.devicekey {
                map.insert(d.deviceid.clone(), k.clone());
            }
        }
        if !map.is_empty() {
            save_device_keys(&map);
        }
    }
    let missing: Vec<&String> = deviceids.iter().filter(|d| !map.contains_key(*d)).collect();
    if !missing.is_empty() {
        bail!(
            "未取得 {} 的 devicekey（局域网控制需要它：请先 ewelink-rs login）",
            missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aes_roundtrip() {
        let key = aes_key("de112c32-682f-460b-b63d-64479c48b5c7");
        let (data, iv) = aes_cbc_encrypt(&json!({"switches":[{"switch":"on","outlet":0}]}), &key).unwrap();
        let pt = aes_cbc_decrypt(&data, &iv, &key).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&pt).unwrap(),
            json!({"switches":[{"switch":"on","outlet":0}]})
        );
    }
}
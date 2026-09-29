//! 视觉监控：读取 visiond 写入的共享内存（布局见下方常量）。
//!
//! SHM 布局（C 结构，小端，总长 2MB）：
//! ```text
//! 0    sequence_num  u64
//! 8    timestamp_ms  u64
//! 16   num_detections u32
//! 20   detections[10]  (each: class_id u32, score f32, x1,y1,x2,y2 f32) = 24B
//! 260  frame_jpeg_size u32
//! 264  frame_jpeg[...]  JPEG 或 YUYV 原始帧 (1280x720x2 = 1843200B)
//! 1999996  format_magic u32 ('YUYV' = YUYV 帧模式; 0 = JPEG/调试模式)
//! ```
//!
//! 调试模式（WEB_VISION_DEBUG=1，仅开发机）：visiond 未运行/未创建 SHM 时，
//! webd 自建 SHM 并用固定测试帧 + 模拟检测结果每 1s 更新，供前端联调。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};


pub const SHM_NAME: &str = "/visiond_detect";
const OFF_SEQ: usize = 0;
const OFF_TS: usize = 8;
const OFF_NUM: usize = 16;
const OFF_DETS: usize = 20;
const DET_SIZE: usize = 24;
const OFF_JPEG_SIZE: usize = 260;
const OFF_JPEG: usize = 264;
const JPEG_CAP: usize = 1_900_000;
/// V4L2 直采模式: SHM 尾部写此 magic, jpeg 字段实为 YUYV 原始帧 (1280x720)
const SHM_FMT_YUYV: u32 = 0x5659_5559;
/// 格式魔数偏移 = SHM 末尾 4 字节。头部没有空位 (+12 与 timestamp u64@8..16 重叠互踩),
/// YUYV 帧数据止于 264+1843200=1843464, 之后到 SHM 末尾均空闲
const OFF_FMT: usize = SHM_SIZE - 4;
const CAM_W: usize = 1280;
const CAM_H: usize = 720;
pub const SHM_SIZE: usize = 2_000_000;

const DEBUG_JPEG: &[u8] = include_bytes!("../web/debug_frame.jpg");

/// SHM timestamp_ms 是 vdec 用 CLOCK_MONOTONIC 写入的单调毫秒。
/// 超过该时长没有新帧即视为“画面停滞/陈旧帧”。
const STALE_MS: u64 = 5_000;

/// 当前 CLOCK_MONOTONIC 毫秒（与 vdec SHM timestamp_ms 同源，可直接比大小）
fn mono_now_ms() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as u64) * 1000 + (ts.tv_nsec as u64) / 1_000_000
}

/// 检测结果（解码后）
#[derive(Clone, Debug)]
pub struct Detection {
    pub class_id: u32,
    pub score: f32,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

/// 一帧快照
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub sequence: u64,
    pub is_yuyv: bool,
    pub timestamp_ms: u64,
    pub detections: Vec<Detection>,
    pub jpeg: Option<Vec<u8>>,
}

pub fn detection_to_json(d: &Detection) -> Value {
    json!({
        "class_id": d.class_id,
        "class": class_name(d.class_id),
        "score": d.score,
        "bbox": [d.x1, d.y1, d.x2, d.y2],
    })
}

pub fn snapshot_to_json(s: &Snapshot) -> Value {
    json!({
        "sequence": s.sequence,
        "timestamp_ms": s.timestamp_ms,
        "stale": is_stale(s),
        "detections": s.detections.iter().map(detection_to_json).collect::<Vec<_>>(),
    })
}

/// 帧是否已陈旧（超过 STALE_MS 没有新帧）
pub fn is_stale(s: &Snapshot) -> bool {
    mono_now_ms().saturating_sub(s.timestamp_ms) > STALE_MS
}

pub fn class_name(id: u32) -> &'static str {
    match id {
        0 => "cat",
        1 => "dog",
        2 => "person",
        _ => "unknown",
    }
}

// ---------- 原始缓冲区读写（小端） ----------

fn read_u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}
fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}
fn read_f32(buf: &[u8], off: usize) -> f32 {
    f32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}
fn write_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
fn write_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn write_f32(buf: &mut [u8], off: usize, v: f32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

/// 从完整缓冲区解码一帧（长度不足视为损坏）
pub fn decode(buf: &[u8]) -> Option<Snapshot> {
    if buf.len() < SHM_SIZE {
        return None;
    }
    let num = read_u32(buf, OFF_NUM).min(10) as usize;
    let mut detections = Vec::with_capacity(num);
    for i in 0..num {
        let base = OFF_DETS + i * DET_SIZE;
        detections.push(Detection {
            class_id: read_u32(buf, base),
            score: read_f32(buf, base + 4),
            x1: read_f32(buf, base + 8),
            y1: read_f32(buf, base + 12),
            x2: read_f32(buf, base + 16),
            y2: read_f32(buf, base + 20),
        });
    }
    let fmt_magic = read_u32(buf, OFF_FMT);
    let is_yuyv = fmt_magic == SHM_FMT_YUYV;
    let jsize = read_u32(buf, OFF_JPEG_SIZE) as usize;
    // YUYV 大帧(1.84MB)不拷入 Snapshot —— frame() 端点经 with_shm 零拷贝直读
    let jpeg = if !is_yuyv && jsize > 0 && jsize <= JPEG_CAP {
        Some(buf[OFF_JPEG..OFF_JPEG + jsize].to_vec())
    } else {
        None
    };
    Some(Snapshot {
        sequence: read_u64(buf, OFF_SEQ),
        timestamp_ms: read_u64(buf, OFF_TS),
        is_yuyv,
        detections,
        jpeg,
    })
}

/// 编码一帧到缓冲区（调试写入/测试用）
pub fn encode(buf: &mut [u8], snap: &Snapshot) {
    assert!(buf.len() >= SHM_SIZE);
    write_u64(buf, OFF_SEQ, snap.sequence);
    write_u64(buf, OFF_TS, snap.timestamp_ms);
    write_u32(buf, OFF_NUM, snap.detections.len().min(10) as u32);
    for (i, d) in snap.detections.iter().take(10).enumerate() {
        let base = OFF_DETS + i * DET_SIZE;
        write_u32(buf, base, d.class_id);
        write_f32(buf, base + 4, d.score);
        write_f32(buf, base + 8, d.x1);
        write_f32(buf, base + 12, d.y1);
        write_f32(buf, base + 16, d.x2);
        write_f32(buf, base + 20, d.y2);
    }
    match &snap.jpeg {
        Some(j) if j.len() <= JPEG_CAP => {
            write_u32(buf, OFF_JPEG_SIZE, j.len() as u32);
            buf[OFF_JPEG..OFF_JPEG + j.len()].copy_from_slice(j);
        }
        _ => write_u32(buf, OFF_JPEG_SIZE, 0),
    }
}

// ---------- SHM 打开/读取 ----------

/// 打开并映射 SHM，读取一帧（每次独立 open/mmap/munmap，无跨 await 状态）

// ---------- 零拷贝 SHM 访问 ----------
// 常驻 mmap 视图: 进程生命周期内映射一次, 后续读取零拷贝
// (旧实现每次调用 mmap+copy 2MB+munmap, WS 10Hz + 轮询叠加 ~40MB/s 无效拷贝)
struct ShmMap {
    ptr: *const u8,
    len: usize,
    ino: u64,
}
unsafe impl Send for ShmMap {}
unsafe impl Sync for ShmMap {}
impl ShmMap {
    fn slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}
static SHM_MAP: std::sync::Mutex<Option<ShmMap>> = std::sync::Mutex::new(None);

/// 段的当前状态: 存在返回 (inode, size)
/// 注意 shm_open 的名字映射到 /dev/shm/<name>, stat 必须用完整路径
fn shm_stat() -> Option<(u64, i64)> {
    let path = std::ffi::CString::new(format!("/dev/shm{}", SHM_NAME)).ok()?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    unsafe {
        if libc::stat(path.as_ptr(), &mut st) != 0 {
            None
        } else {
            Some((st.st_ino, st.st_size))
        }
    }
}

/// 在 SHM 映射上执行 f; 未附着则惰性附着。
/// 每次调用校验段 inode: visiond 重启会 unlink+新建段, 旧 mmap 会变成永远读死数据的悬空映射
/// (曾导致重启后画面/检测冻结在最后一帧), 校验后自动重映射; 段消失则解绑并报不可用。
fn with_shm<T>(f: impl FnOnce(&[u8]) -> T) -> Option<T> {
    let mut g = SHM_MAP.lock().ok()?;
    let cur = shm_stat();
    match (&*g, cur) {
        (Some(m), None) => {
            unsafe { libc::munmap(m.ptr as *mut _, m.len) }; // 段已删除: 解绑
            *g = None;
        }
        (Some(m), Some((ino, sz))) => {
            if ino != m.ino || sz < SHM_SIZE as i64 {
                unsafe { libc::munmap(m.ptr as *mut _, m.len) }; // 段被重建: 重映射
                *g = None;
            }
        }
        _ => {}
    }
    if g.is_none() {
        let (ino, _) = cur?;
        unsafe {
            let cname = std::ffi::CString::new(SHM_NAME).ok()?;
            let fd = match libc::shm_open(cname.as_ptr(), libc::O_RDONLY, 0o666) {
                f if f >= 0 => f,
                _ => return None,
            };
            let mut st: libc::stat = std::mem::zeroed();
            if libc::fstat(fd, &mut st) != 0 || st.st_size < SHM_SIZE as i64 {
                libc::close(fd);
                return None; // vdec 尚未创建足够大的 SHM
            }
            let ptr = libc::mmap(
                std::ptr::null_mut(),
                SHM_SIZE,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            );
            libc::close(fd);
            if ptr == libc::MAP_FAILED {
                return None;
            }
            *g = Some(ShmMap { ptr: ptr as *const u8, len: SHM_SIZE, ino });
        }
    }
    g.as_ref().map(|m| f(m.slice()))
}

pub fn try_read() -> Option<Snapshot> {
    with_shm(|buf| decode(buf)).flatten()
}

/// 只读映射整个 SHM；不存在返回 None
fn map_shm(_extra: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let cname = std::ffi::CString::new(SHM_NAME).ok()?;
        let fd = libc::shm_open(cname.as_ptr(), libc::O_RDONLY, 0o666);
        if fd < 0 {
            return None;
        }
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut st) != 0 || st.st_size < SHM_SIZE as i64 {
            libc::close(fd);
            return None;
        }
        let ptr = libc::mmap(
            std::ptr::null_mut(),
            SHM_SIZE,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
            0,
        );
        libc::close(fd);
        if ptr == libc::MAP_FAILED {
            return None;
        }
        let mut out = vec![0u8; SHM_SIZE];
        std::ptr::copy_nonoverlapping(ptr as *const u8, out.as_mut_ptr(), SHM_SIZE);
        libc::munmap(ptr, SHM_SIZE);
        Some(out)
    }
}

// ---------- 调试模式 writer ----------

pub struct DebugWriter {
    stop: Arc<AtomicBool>,
}

impl DebugWriter {
    /// 启动调试 writer：自建 SHM（visiond 已存在时跳过），每 1s 写一帧模拟数据。
    /// 返回 None 表示 SHM 已被 visiond 占用（用真实数据即可）。
    pub fn start() -> Option<DebugWriter> {
        create_debug_shm()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let mut seq: u64 = 0;
        std::thread::spawn(move || {
            let mut last_write = vec![0u8; SHM_SIZE];
            // 用 include_bytes 的静态帧直接映射（避免每次拷贝）
            let frame: &[u8] = DEBUG_JPEG;
            while !stop2.load(Ordering::Relaxed) {
                seq += 1;
                let snap = Snapshot {
                    sequence: seq,
                    is_yuyv: false,
                    timestamp_ms: crate::push::now_ms(),
                    detections: vec![
                        Detection { class_id: 0, score: 0.92, x1: 0.10, y1: 0.20, x2: 0.55, y2: 0.62 },
                        Detection { class_id: 1, score: 0.78, x1: 0.45, y1: 0.60, x2: 0.90, y2: 0.95 },
                    ],
                    jpeg: Some(frame.to_vec()),
                };
                encode(&mut last_write, &snap);
                write_shm(&last_write);
                std::thread::sleep(Duration::from_secs(1));
            }
        });
        Some(DebugWriter { stop })
    }

    pub fn is_debug_active(&self) -> bool {
        !self.stop.load(Ordering::Relaxed)
    }
}

impl Drop for DebugWriter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        unsafe {
            let cname = std::ffi::CString::new(SHM_NAME).unwrap();
            libc::shm_unlink(cname.as_ptr());
        }
    }
}

/// 创建调试 SHM：先 unlink 遗留段（上次异常退出的残留），再 O_CREAT|O_EXCL。
/// 注意：调试模式仅开发机使用，与真 visiond 同机运行时不保证数据正确。
fn create_debug_shm() -> Option<()> {
    unsafe {
        let cname = std::ffi::CString::new(SHM_NAME).ok()?;
        libc::shm_unlink(cname.as_ptr()); // 忽略错误：不存在或已由 visiond 创建
        let fd = libc::shm_open(cname.as_ptr(), libc::O_CREAT | libc::O_EXCL | libc::O_RDWR, 0o666);
        if fd < 0 {
            return None; // 创建失败（正常路径不会走到，除非 /dev/shm 不存在）
        }
        if libc::ftruncate(fd, SHM_SIZE as i64) != 0 {
            libc::close(fd);
            libc::shm_unlink(cname.as_ptr());
            return None;
        }
        libc::close(fd);
        Some(())
    }
}

fn write_shm(buf: &[u8]) {
    unsafe {
        let cname = std::ffi::CString::new(SHM_NAME).unwrap();
        let fd = libc::shm_open(cname.as_ptr(), libc::O_RDWR, 0o666);
        if fd < 0 {
            return;
        }
        let ptr = libc::mmap(
            std::ptr::null_mut(),
            SHM_SIZE,
            libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        );
        libc::close(fd);
        if ptr == libc::MAP_FAILED {
            return;
        }
        std::ptr::copy_nonoverlapping(buf.as_ptr(), ptr as *mut u8, SHM_SIZE);
        libc::munmap(ptr, SHM_SIZE);
    }
}

// ---------- HTTP / WS handler ----------

/// GET /api/vision/meta
pub async fn meta(debug: Option<Arc<DebugWriter>>) -> Value {
    let snap = try_read();
    let real = snap.is_some();
    let stale = snap.as_ref().map(is_stale).unwrap_or(false);
    json!({
        "available": real,
        "debug": debug.is_some(),
        "stale": stale,
        "sequence": snap.map(|s| s.sequence),
    })
}

/// GET /api/vision/latest
pub async fn latest() -> Response {
    match try_read() {
        Some(snap) => axum::Json(snapshot_to_json(&snap)).into_response(),
        None => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "视觉服务未运行（visiond 未写入 SHM）",
        )
            .into_response(),
    }
}

/// GET /api/vision/frame → image/jpeg
pub async fn frame() -> Response {
    // with_shm 零拷贝直读: 缓存检查 + YUYV→RGB→JPEG 全在映射上完成,
    // 唯一拷贝是输出 JPEG (~60KB); 同 sequence 命中缓存零转码
    let out = with_shm(|buf| {
        let seq = read_u64(buf, OFF_SEQ);
        if seq > 0 {
            if let Ok(cached) = JPG_CACHE.lock() {
                if let Some((cseq, jpg)) = cached.as_ref() {
                    if *cseq == seq {
                        return jpg.clone();
                    }
                }
            }
        }
        let fmt_magic = read_u32(buf, OFF_FMT);
        if fmt_magic != SHM_FMT_YUYV {
            // JPEG 模式 (FIFO/debug): 直接取
            let jsize = read_u32(buf, OFF_JPEG_SIZE) as usize;
            return if jsize > 0 && jsize <= buf.len() - OFF_JPEG {
                Some(buf[OFF_JPEG..OFF_JPEG + jsize].to_vec())
            } else {
                None
            };
        }
        // YUYV → RGB → JPEG
        let dlen = CAM_W * CAM_H * 2;
        if buf.len() < OFF_JPEG + dlen {
            return None;
        }
        encode_yuyv_jpeg_from_data(&buf[OFF_JPEG..OFF_JPEG + dlen])
    });
    match out.flatten() {
        Some(jpeg) => (
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "image/jpeg")],
            jpeg,
        )
            .into_response(),
        None => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "no frame",
        )
            .into_response(),
    }
}

/// 转码缓存: (sequence, jpeg) —— 多客户端/高频轮询共享, 同帧不重复编码
static JPG_CACHE: std::sync::Mutex<Option<(u64, Option<Vec<u8>>)>> = std::sync::Mutex::new(None);

/// 从快照取可展示的 JPEG：JPEG 直返；YUYV 帧按需转换编码（仅有人看页面时消耗 CPU）

/// YUYV 原始帧 → JPEG 编码
fn encode_yuyv_jpeg_from_data(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < CAM_W * CAM_H * 2 {
        return None;
    }
    let mut rgb = vec![0u8; CAM_W * CAM_H * 3];
    for row in 0..CAM_H {
        let src = &data[row * CAM_W * 2..];
        for x in 0..CAM_W / 2 {
            let base = x * 4;
            let yv0 = src[base] as i32;
            let u = src[base + 1] as i32 - 128;
            let yv1 = src[base + 2] as i32;
            let v = src[base + 3] as i32 - 128;
            let (dr, dg, db) = (uv_term(u, v));
            let (r0, g0, b0) = clamp_yuv(yv0 + dr, yv0 + dg, yv0 + db);
            let doff = (row * CAM_W + x * 2) * 3;
            rgb[doff..doff + 3].copy_from_slice(&[r0, g0, b0]);
            let (r1, g1, b1) = clamp_yuv(yv1 + dr, yv1 + dg, yv1 + db);
            rgb[doff + 3..doff + 6].copy_from_slice(&[r1, g1, b1]);
        }
    }
    let mut out = Vec::with_capacity(64 * 1024);
    let mut enc = jpeg_encoder::Encoder::new(&mut out, 70);
    enc.encode(&rgb, CAM_W as u16, CAM_H as u16, jpeg_encoder::ColorType::Rgb)
        .ok()?;
    Some(out)
}

#[inline]
fn uv_term(u: i32, v: i32) -> (i32, i32, i32) {
    ((91881 * v) >> 16, (-22554 * u - 46802 * v) >> 16, (116130 * u) >> 16)
}
#[inline]
fn clamp_yuv(r: i32, g: i32, b: i32) -> (u8, u8, u8) {
    (
        r.clamp(0, 255) as u8,
        g.clamp(0, 255) as u8,
        b.clamp(0, 255) as u8,
    )
}

/// GET /api/vision/stream.mjpg → MJPEG multipart 流。
/// 浏览器 <img> 原生消费（GPU 合成解码），前端零 JS 搬运 —— 移动端流畅的关键。
pub async fn stream_mjpg() -> Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(4);
    tokio::spawn(async move {
        let mut last_seq: Option<u64> = None;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tick.tick().await;
            if let Some(snap) = try_read() {
                let changed = last_seq.map(|s| s != snap.sequence).unwrap_or(true);
                last_seq = Some(snap.sequence);
                if changed && !snap.is_yuyv {
                    if let Some(jpeg) = snap.jpeg {
                        let mut buf = format!(
                            "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                            jpeg.len()
                        )
                        .into_bytes();
                        buf.extend_from_slice(&jpeg);
                        buf.extend_from_slice(b"\r\n");
                        if tx.send(Ok(bytes::Bytes::from(buf))).await.is_err() {
                            break; // 客户端断开
                        }
                    }
                }
            }
        }
    });
    Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(
            axum::http::header::CONTENT_TYPE,
            "multipart/x-mixed-replace; boundary=frame",
        )
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .body(axum::body::Body::from_stream(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        ))
        .unwrap()
}

/// GET /api/vision/ws → 每帧推送（500ms 轮询 SHM sequence_num）
pub async fn ws(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(ws_loop)
}

async fn ws_loop(mut socket: WebSocket) {
    let mut last_seq: Option<u64> = None;
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(p))) => {
                        if socket.send(Message::Pong(p)).await.is_err() { break; }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            _ = interval.tick() => {
                let Some(snap) = try_read() else { continue };
                if last_seq == Some(snap.sequence) { continue; }
                last_seq = Some(snap.sequence);
                let text = snapshot_to_json(&snap).to_string();
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
                // 注意: YUYV 模式下 jpeg 字段是原始帧(1.84MB), 不能走 WS;
                // 画面由前端轮询 /api/vision/frame (服务端按需转 JPEG + 缓存)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let mut buf = vec![0u8; SHM_SIZE];
        let snap = Snapshot {
            sequence: 42,
            is_yuyv: false,
            timestamp_ms: 123456789,
            detections: vec![
                Detection { class_id: 0, score: 0.92, x1: 0.1, y1: 0.2, x2: 0.5, y2: 0.6 },
                Detection { class_id: 1, score: 0.78, x1: 0.4, y1: 0.5, x2: 0.9, y2: 0.95 },
            ],
            jpeg: Some(vec![0xFFu8, 0xD8, 0xFF, 0xE0, 0x00]),
        };
        encode(&mut buf, &snap);
        let out = decode(&buf).unwrap();
        assert_eq!(out.sequence, 42);
        assert_eq!(out.timestamp_ms, 123456789);
        assert_eq!(out.detections.len(), 2);
        assert_eq!(out.detections[0].class_id, 0);
        assert!((out.detections[1].score - 0.78).abs() < 1e-6);
        assert_eq!(out.jpeg, Some(vec![0xFFu8, 0xD8, 0xFF, 0xE0, 0x00]));
    }

    #[test]
    fn decode_truncated_returns_none() {
        assert!(decode(&vec![0u8; 100]).is_none());
    }

    #[test]
    fn num_detections_capped_at_10() {
        let mut buf = vec![0u8; SHM_SIZE];
        let snap = Snapshot {
            sequence: 1,
            is_yuyv: false,
            timestamp_ms: 1,
            detections: (0..15).map(|i| Detection { class_id: i % 2, score: 0.5, x1: 0.0, y1: 0.0, x2: 0.1, y2: 0.1 }).collect(),
            jpeg: None,
        };
        encode(&mut buf, &snap);
        assert_eq!(decode(&buf).unwrap().detections.len(), 10);
    }

    #[test]
    fn offsets_match_design_doc() {
        // num_detections@16, detections@20 (24B × 10), jpeg_size@260, jpeg@264
        assert_eq!(OFF_NUM, 16);
        assert_eq!(OFF_DETS, 20);
        assert_eq!(DET_SIZE, 24);
        assert_eq!(OFF_JPEG_SIZE, 260);
        assert_eq!(OFF_JPEG, 264);
        // 魔数在 SHM 尾部: 头部 +12 与 timestamp u64@8..16 重叠, 不能放
        assert_eq!(OFF_FMT, SHM_SIZE - 4);
        assert_eq!(OFF_FMT, 1_999_996);
        assert_eq!(SHM_FMT_YUYV, 0x5659_5559);
        assert_eq!(SHM_SIZE, 2_000_000);
    }

    #[test]
    fn yuyv_frame_fits_shm() {
        // YUYV 原始帧必须完整放进 jpeg 字段且不越过魔数区
        let frame = CAM_W * CAM_H * 2;
        assert_eq!(frame, 1_843_200);
        assert!(OFF_JPEG + frame <= OFF_FMT);
    }
}
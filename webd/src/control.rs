//! 自动控制（喂狗）：
//!   触发条件：画面中狗连续出现 ≥ dog_seconds 秒，且窗口内不存在猫
//!   配额：每日 quota 次，每天 refresh_hour 点（按 WEB_CTRL_TZ 时区）整点重置
//!   动作：插座 pulse 通电 socket_seconds 秒 + 邮件通知（附消耗瞬间实时照片）
//!
//! 状态机：SHM 每帧（seq 变化）步进一次；狗且无猫 → 累计连续时长，跨过阈值即触发；
//! 出现猫或狗消失 → 累计清零。配额用尽时记录但不动作。

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::time::sleep;

use crate::config::Config;
use crate::{ewelink, push, vision};

pub const CLASS_CAT: u32 = 0;
pub const CLASS_DOG: u32 = 1;

/// 配额用尽时创建该文件，vdec_stream 检测到后暂停推理链（节省 JPU/TPU）
const PAUSE_FILE: &str = "/tmp/vdec_pause";
/// 配额用尽且开启“保留视频流”时创建：vdec 只停推理，继续采集发布实时画面
const KEEP_VIDEO_FILE: &str = "/tmp/vdec_keep_video";

/// 触发窗口（秒）：狗需在窗口内持续出现。**固定常量**——不可由环境变量/页面调整。
pub const DOG_WINDOW_S: f32 = 1.2;
/// 触发阈值：窗口内狗置信度均值须大于该值（0.65 = 65%）。**固定常量**。
pub const DOG_MIN_SCORE: f32 = 0.65;

/// 管理台参数配置请求（全部可选，只更新提供的字段）。
/// 注意：狗窗口(DOG_WINDOW_S=1.2s)与置信度阈值(DOG_MIN_SCORE=0.65)为**代码内固定常量**，
/// 既不允许页面动态修改，也不受环境变量影响。
#[derive(serde::Deserialize, Default)]
pub struct ConfigReq {
    pub quota: Option<u32>,
    pub socket_seconds: Option<u32>,
    /// 兼容旧接口：单个整点（等价于 refresh_hours=[v]）
    pub refresh_hour: Option<u32>,
    /// 多整点刷新（0..=23，自动排序去重；至少一个）
    pub refresh_hours: Option<Vec<u32>>,
    /// 配额用尽时是否保留实时视频流
    pub keep_video_on_pause: Option<bool>,
}

/// 控制状态（配置 + 运行时 + 持久化配额）
pub struct CtrlState {
    // 配置镜像（供 /api/control/status 展示）
    pub enabled: bool,
    pub dog_seconds: f32,
    pub dog_min_score: f32,
    pub quota: u32,
    /// 配额刷新整点列表（0..=23，排序去重，至少一个）
    pub refresh_hours: Vec<u32>,
    pub socket_seconds: u32,
    /// 配额用尽时是否保留实时视频流（true=保留画面只停推理；false=连摄像头/视频一起关）
    pub keep_video_on_pause: bool,
    pub socket_id: String,
    /// None = 全部插位（固件虚报多通道、物理仅一个插座）
    pub socket_outlet: Option<u8>,
    pub tz_label: String,
    pub tz_offset: i64,
    pub state_file: std::path::PathBuf,
    pub shot_path: std::path::PathBuf,
    // 运行时
    /// 调试模式强制开启推理+视频流（忽略配额用尽暂停；不持久化）
    pub force_video: bool,
    /// 滑动窗口样本：(帧时间戳 ms, 该帧狗最高置信度)
    pub dog_window: Vec<(u64, f32)>,
    /// 最近猫帧时间戳 ms（0 = 未见猫）
    pub last_cat_ms: u64,
    pub last_frame_ms: u64,
    pub last_seq: u64,
    // 配额（持久化）
    pub used_today: u32,
    pub last_quota_day: i64,
    pub last_trigger_ms: u64,
    pub last_action: String,
}

impl CtrlState {
    pub fn from_cfg(cfg: &Config) -> Self {
        let mut s = CtrlState {
            enabled: cfg.ctrl_enabled,
            dog_seconds: DOG_WINDOW_S,
            dog_min_score: DOG_MIN_SCORE,
            quota: cfg.ctrl_quota,
            refresh_hours: vec![cfg.ctrl_refresh_hour.min(23)],
            socket_seconds: cfg.ctrl_socket_seconds,
            keep_video_on_pause: cfg.ctrl_keep_video_on_pause,
            socket_id: cfg.ctrl_socket_id.clone(),
            socket_outlet: cfg.ctrl_socket_outlet,
            tz_label: tz_label(cfg.ctrl_tz_offset),
            tz_offset: cfg.ctrl_tz_offset,
            state_file: cfg.ctrl_state_file.clone(),
            shot_path: cfg.ctrl_shot_path.clone(),
            dog_window: Vec::new(),
            last_cat_ms: 0,
            last_frame_ms: 0,
            last_seq: 0,
            force_video: false,
            used_today: 0,
            last_quota_day: 0,
            last_trigger_ms: 0,
            last_action: String::new(),
        };
        if let Some(v) = load_state(&s.state_file) {
            s.used_today = v_u32(&v, "used_today", 0);
            s.last_quota_day = v.get("last_quota_day").and_then(Value::as_i64).unwrap_or(0);
            s.quota = v_u32(&v, "quota", s.quota).clamp(1, 1000);
            s.socket_seconds = v_u32(&v, "socket_seconds", s.socket_seconds).clamp(1, 3600);
            s.keep_video_on_pause = v.get("keep_video_on_pause").and_then(Value::as_bool).unwrap_or(s.keep_video_on_pause);
            // 多整点优先；兼容旧单点 refresh_hour 字段
            if let Some(hs) = v.get("refresh_hours").and_then(Value::as_array) {
                let hs: Vec<u32> = hs.iter().filter_map(|x| x.as_u64()).map(|x| x as u32).filter(|h| *h <= 23).collect();
                if !hs.is_empty() {
                    s.refresh_hours = normalize_hours(&hs).unwrap_or(s.refresh_hours);
                }
            } else if let Some(h) = v.get("refresh_hour").and_then(Value::as_u64) {
                s.refresh_hours = vec![(h as u32).min(23)];
            }
            // 注意：dog_seconds / dog_min_score 为代码内固定常量（DOG_WINDOW_S / DOG_MIN_SCORE），
            // 旧状态文件里残留的值一律忽略
        }
        s.sync_pause();
        s
    }

    /// 运行时可配置参数（管理台设置页）：校验后写入并持久化。
    /// dog_seconds / dog_min_score 为代码内固定常量，不在此列。
    pub fn update_config(&mut self, q: &ConfigReq) -> Result<Value, String> {
        if let Some(v) = q.quota {
            if !(1..=1000).contains(&v) { return Err(format!("配额须在 1..=1000（收到 {v}）")); }
            self.quota = v;
        }
        if let Some(v) = q.socket_seconds {
            if !(1..=3600).contains(&v) { return Err(format!("通电时长须在 1..=3600 秒（收到 {v}）")); }
            self.socket_seconds = v;
        }
        if let Some(hs) = &q.refresh_hours {
            self.refresh_hours = normalize_hours(hs)?;
        } else if let Some(v) = q.refresh_hour {
            if v > 23 { return Err(format!("刷新时刻须在 0..=23 时（收到 {v}）")); }
            self.refresh_hours = vec![v];
        }
        if let Some(v) = q.keep_video_on_pause {
            self.keep_video_on_pause = v;
        }
        // 刷新时刻变更后立即对齐当前槽位，避免切换时产生一次多余重置
        if q.refresh_hours.is_some() || q.refresh_hour.is_some() {
            self.last_quota_day = Self::quota_slot_index(unix_now(), self.tz_offset, &self.refresh_hours);
        }
        self.persist();
        self.sync_pause();
        Ok(json!({
            "ok": true,
            "quota": self.quota,
            "socket_seconds": self.socket_seconds,
            "dog_seconds": self.dog_seconds,
            "dog_min_score": self.dog_min_score,
            "refresh_hour": self.refresh_hours.first().copied().unwrap_or(6),
            "refresh_hours": self.refresh_hours,
            "keep_video_on_pause": self.keep_video_on_pause,
        }))
    }

    /// 配额槽索引：从 epoch 起，每遇到一个配置的整点就 +1。
    /// 索引变化即代表"到点发放新配额"。支持一天多个整点。
    fn quota_slot_index(unix: i64, tz: i64, hours: &[u32]) -> i64 {
        if hours.is_empty() {
            return unix.div_euclid(86400);
        }
        let local = unix + tz;
        let day = local.div_euclid(86400);
        let sec = local.rem_euclid(86400);
        let n = hours.iter().filter(|h| (**h as i64) * 3600 <= sec).count() as i64;
        day * (hours.len() as i64) + n
    }

    /// 系统时钟是否可信。上电时 RTC 默认 1970，NTP 同步前日历无意义。
    /// 此时若照常刷新，会：
    ///   1) 误响一声配额提醒（NTP 再同步一次又响一声 → 开机固定响两下）；
    ///   2) 把 used_today 清零 —— 等于"重板子就能绕过每日限额"。
    fn clock_valid(unix: i64) -> bool {
        const Y2020: i64 = 1_577_836_800; // 2020-01-01 00:00:00 UTC
        unix >= Y2020
    }

    /// 配额刷新检查（每次轮询调用，便宜）。返回 true = 本次发生了配额刷新。
    pub fn refresh_if_due(&mut self, tz: i64) -> bool {
        let unix = unix_now();
        // 时钟未同步：不刷新、不清零、不响；等 NTP 校准后再比对真实槽位
        if !Self::clock_valid(unix) {
            return false;
        }
        let qd = Self::quota_slot_index(unix, tz, &self.refresh_hours);
        if qd != self.last_quota_day {
            self.last_quota_day = qd;
            self.used_today = 0;
            self.persist();
            self.sync_pause();
            true
        } else {
            false
        }
    }

    /// 一帧一步（滑动窗口判定）。返回 Some(快照) = 本帧触发（已消耗配额）。
    /// 触发条件：窗口(dog_seconds)内狗置信度均值 > dog_min_score 且窗口内无猫。
    pub fn step(&mut self, snap: &vision::Snapshot, tz: i64) -> Option<vision::Snapshot> {
        if !self.enabled || snap.sequence == self.last_seq {
            return None;
        }
        self.last_seq = snap.sequence;
        self.last_frame_ms = snap.timestamp_ms;
        let now = snap.timestamp_ms;
        let win_ms = ((self.dog_seconds * 1000.0).max(1.0)) as u64;

        // 清理窗口外样本（边界样本保留：跨度恰好 = win_ms 也满足触发条件）
        self.dog_window.retain(|(t, _)| now.saturating_sub(*t) <= win_ms);

        let best_dog = snap
            .detections
            .iter()
            .filter(|d| d.class_id == CLASS_DOG)
            .map(|d| d.score)
            .fold(0.0f32, f32::max);
        let cat = snap.detections.iter().any(|d| d.class_id == CLASS_CAT);

        if best_dog > 0.0 {
            self.dog_window.push((now, best_dog));
        }
        if cat {
            self.last_cat_ms = now;
        }
        // 猫出现在窗口内 → 判定失败，清窗重来（"狗高置信且无猫"）。
        // last_cat_ms==0 表示从未见猫，跳过（否则 now-0 < win_ms 在前段恒真误清窗）
        if self.last_cat_ms > 0 && now.saturating_sub(self.last_cat_ms) < win_ms {
            self.dog_window.clear();
            return None;
        }
        // 窗口跨度覆盖 dog_seconds，且均值 > 阈值 → 触发
        let span = self
            .dog_window
            .first()
            .and_then(|(t0, _)| self.dog_window.last().map(|(t1, _)| t1.saturating_sub(*t0)))
            .unwrap_or(0);
        if span >= win_ms && !self.dog_window.is_empty() {
            let avg: f32 =
                self.dog_window.iter().map(|(_, s)| *s).sum::<f32>() / self.dog_window.len() as f32;
            if avg > self.dog_min_score {
                self.dog_window.clear();
                self.refresh_if_due(tz);
                if self.used_today < self.quota {
                    self.used_today += 1;
                    self.last_trigger_ms = now;
                    self.persist();
                    self.sync_pause();
                    return Some(snap.clone());
                }
                self.last_action = format!(
                    "狗持续出现但配额已用尽 ({}/{})", self.used_today, self.quota
                );
            }
        }
        None
    }

    pub fn reset_quota(&mut self) {
        self.used_today = 0;
        self.persist();
        self.clear_pause();
    }

    /// 配额用尽 → 写暂停标志；有剩余 → 清除暂停标志。
    /// 调试模式强制开启推理+视频流时，始终清除暂停标志。
    pub fn sync_pause(&self) {
        if self.force_video {
            let _ = std::fs::remove_file(PAUSE_FILE);
            let _ = std::fs::remove_file(KEEP_VIDEO_FILE);
            return;
        }
        if self.used_today >= self.quota {
            let _ = std::fs::write(PAUSE_FILE, b"");
            if self.keep_video_on_pause {
                let _ = std::fs::write(KEEP_VIDEO_FILE, b"");
            } else {
                let _ = std::fs::remove_file(KEEP_VIDEO_FILE);
            }
        } else {
            let _ = std::fs::remove_file(PAUSE_FILE);
            let _ = std::fs::remove_file(KEEP_VIDEO_FILE);
        }
    }

    pub fn inference_paused(&self) -> bool {
        !self.force_video && self.used_today >= self.quota
    }

    /// 调试模式开关：开启=强制推理+视频流，关闭=按配额状态恢复暂停
    pub fn set_force_video(&mut self, on: bool) {
        self.force_video = on;
        self.sync_pause();
    }

    fn clear_pause(&self) {
        let _ = std::fs::remove_file(PAUSE_FILE);
        let _ = std::fs::remove_file(KEEP_VIDEO_FILE);
    }

    pub fn persist(&self) {
        let v = json!({
            "used_today": self.used_today,
            "last_quota_day": self.last_quota_day,
            "last_trigger_ms": self.last_trigger_ms,
            "quota": self.quota,
            "refresh_hour": self.refresh_hours.first().copied().unwrap_or(6),
            "refresh_hours": self.refresh_hours,
            "socket_seconds": self.socket_seconds,
            "keep_video_on_pause": self.keep_video_on_pause,
        });
        if let Some(dir) = self.state_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.state_file, serde_json::to_vec(&v).unwrap_or_default());
    }

    pub fn status_json(&self, vision_online: bool) -> Value {
        json!({
            "enabled": self.enabled,
            "dog_seconds": self.dog_seconds,
            "dog_min_score": self.dog_min_score,
            "dog_rule": format!("窗口 {}s 内狗均值 > {:.0}% 且无猫", self.dog_seconds, self.dog_min_score * 100.0),
            "quota": self.quota,
            "refresh_hour": self.refresh_hours.first().copied().unwrap_or(6),
            "refresh_hours": self.refresh_hours,
            "tz": self.tz_label,
            "socket_seconds": self.socket_seconds,
            "keep_video_on_pause": self.keep_video_on_pause,
            "socket_id": self.socket_id,
            "socket_outlet": self.socket_outlet.map(|o| o.to_string()).unwrap_or_else(|| "all".into()),
            "streak_frames": self.dog_window.len(),
            "streak_ms": self.dog_window.first().and_then(|(t0, _)| self.dog_window.last().map(|(t1, _)| t1.saturating_sub(*t0))).unwrap_or(0),
            "used_today": self.used_today,
            "quota_left": self.quota.saturating_sub(self.used_today),
            "inference_paused": self.inference_paused(),
            "last_trigger_ms": self.last_trigger_ms,
            "last_action": self.last_action,
            "vision_online": vision_online,
            "next_refresh": format!("每日 {} ({})", self.refresh_hours.iter().map(|h| format!("{:02}:00", h)).collect::<Vec<_>>().join("/"), self.tz_label),
        })
    }
}

/// 后台任务：轮询 SHM，步进状态机，触发动作
pub async fn run(
    cfg: Arc<Config>,
    state: Arc<Mutex<CtrlState>>,
    dry_run: Arc<std::sync::atomic::AtomicBool>,
) {
    let tz = cfg.ctrl_tz_offset;
    loop {
        {
            let mut st = state.lock().unwrap();
            if st.refresh_if_due(tz) {
                // 配额刷新提醒: 蜂鸣器连续响两下 (异步, 不阻塞轮询)
                tokio::spawn(async {
                    beep_quota_refresh().await;
                });
            }
        }
        if let Some(snap) = vision::try_read() {
            let fire = { state.lock().unwrap().step(&snap, tz) };
            if let Some(snap) = fire {
                let (used, quota, cfg2, dry, sock_sec) = {
                    let st = state.lock().unwrap();
                    (
                        st.used_today,
                        st.quota,
                        cfg.clone(),
                        dry_run.load(std::sync::atomic::Ordering::Relaxed),
                        st.socket_seconds,
                    )
                };
                let st2 = state.clone();
                tokio::spawn(async move {
                    let summary =
                        execute_action(&cfg2, Some(&snap), used, quota, dry, sock_sec).await;
                    let mut st = st2.lock().unwrap();
                    st.last_action = summary.clone();
                    println!("webd: 自动控制动作完成: {summary}");
                });
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// 配额刷新提醒：调用 buzzer_beep.sh 连续响两下
/// （A19/GPIO499 接**高电平触发**模块：高=响，低=静音）
async fn beep_quota_refresh() {
    let bin = Path::new("/usr/bin/buzzer_beep.sh");
    if !bin.exists() {
        println!("webd: buzzer 脚本不存在: {}", bin.display());
        return;
    }
    match tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(bin).arg("2").output(),
    )
    .await
    {
        Ok(Ok(o)) => println!("webd: 配额刷新蜂鸣完成 exit={:?}", o.status.code()),
        Ok(Err(e)) => println!("webd: 配额刷新蜂鸣失败: {e}"),
        Err(_) => println!("webd: 配额刷新蜂鸣超时"),
    }
}

/// 手动测试触发（不消耗配额）：抓当前帧 → 邮件 + 插座
pub async fn manual_test(
    cfg: &Config,
    state: Arc<Mutex<CtrlState>>,
    dry_run: bool,
) -> Value {
    let snap = vision::try_read();
    let shot_ok = snap.as_ref().and_then(|s| s.jpeg.as_ref()).is_some();
    let sock_sec = state.lock().unwrap().socket_seconds;
    let summary = execute_action(cfg, snap.as_ref(), 0, cfg.ctrl_quota, dry_run, sock_sec).await;
    json!({"ok": true, "shot_available": shot_ok, "summary": summary, "dry_run": dry_run})
}

/// 动作链：① 实时照片落盘 → ② 邮件（附照片）→ ③ 插座 pulse 通电
#[allow(clippy::too_many_arguments)]
async fn execute_action(
    cfg: &Config,
    snap: Option<&vision::Snapshot>,
    used: u32,
    quota: u32,
    dry_run: bool,
    socket_seconds: u32,
) -> String {
    let shot = snap
        .and_then(|s| s.jpeg.as_ref())
        .map(|jpg| {
            let p = &cfg.ctrl_shot_path;
            let _ = std::fs::write(p, jpg);
            p.display().to_string()
        });

    let content = format!(
        "{}自动喂狗触发：画面中狗连续出现 ≥ {:.0} 秒且无猫。\n已消耗今日第 {}/{} 次配额。\n插座已通电 {} 秒。",
        if dry_run { "[调试模式-未操作设备] " } else { "" },
        DOG_WINDOW_S, used, quota, socket_seconds
    );
    let mail = push::mailpush(
        cfg,
        push::MailPushReq {
            content,
            title: Some("自动喂狗通知".to_string()),
            image: shot,
        },
    )
    .await;

    let id = if !cfg.ctrl_socket_id.is_empty() {
        cfg.ctrl_socket_id.clone()
    } else {
        ewelink::cached_deviceids(cfg).into_iter().next().unwrap_or_default()
    };
    let sock = if id.is_empty() {
        json!({"error": "无插座 deviceid（未配置 WEB_CTRL_SOCKET_ID 且无缓存设备）"})
    } else if dry_run {
        json!({
            "ok": true,
            "dry_run": true,
            "deviceid": id,
            "outlet": cfg.ctrl_socket_outlet.map(|o| o.to_string()).unwrap_or_else(|| "all".into()),
            "msg": format!("调试模式：已模拟触发插座 pulse（{}s），未实际操作设备", socket_seconds),
        })
    } else {
        ewelink::pulse(
            cfg,
            ewelink::PulseReq {
                deviceid: id,
                ms: (socket_seconds as u64) * 1000,
                outlet: cfg.ctrl_socket_outlet,
            },
        )
        .await
    };
    format!("mail={mail} socket={sock}")
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn tz_label(offset: i64) -> String {
    let sign = if offset < 0 { "-" } else { "+" };
    let a = offset.unsigned_abs();
    format!("UTC{}{:02}:{:02}", sign, a / 3600, (a % 3600) / 60)
}

/// 校验并规范化整点列表：0..=23、至少一个、排序去重。
fn normalize_hours(hours: &[u32]) -> Result<Vec<u32>, String> {
    if hours.is_empty() {
        return Err("至少需要一个刷新时刻".into());
    }
    let mut hs: Vec<u32> = hours.iter().copied().collect();
    for h in &hs {
        if *h > 23 {
            return Err(format!("刷新时刻须在 0..=23 时（收到 {h}）"));
        }
    }
    hs.sort_unstable();
    hs.dedup();
    Ok(hs)
}

fn load_state(p: &Path) -> Option<Value> {
    let raw = std::fs::read(p).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn v_u32(v: &Value, k: &str, d: u32) -> u32 {
    v.get(k).and_then(Value::as_u64).unwrap_or(d as u64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::vision::Detection;

    const TZ: i64 = 8 * 3600;

    fn snap(seq: u64, ts: u64, dets: Vec<(u32, f32)>) -> vision::Snapshot {
        vision::Snapshot {
            sequence: seq,
            is_yuyv: false,
            timestamp_ms: ts,
            detections: dets.into_iter().map(|(c, s)| Detection { class_id: c, score: s, x1: 0., y1: 0., x2: 10., y2: 10. }).collect(),
            jpeg: None,
        }
    }

    fn state(cfg: &Config) -> CtrlState {
        CtrlState::from_cfg(cfg)
    }

    fn cfg_tmp(dir: &tempfile::TempDir) -> Config {
        let mut c = Config::from_env();
        c.ctrl_state_file = dir.path().join("ctl.json");
        c.ctrl_enabled = true;
        c.ctrl_quota = 1;
        c
    }

    #[test]
    fn dog_streak_triggers_at_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_tmp(&dir);
        let mut st = state(&cfg);
        // 10fps 帧: 0,100,...,1100ms → 均不触发; 1200ms 帧触发
        for i in 0..12 {
            let ts = i * 100;
            assert!(st.step(&snap(i + 1, ts, vec![(CLASS_DOG, 0.8)]), TZ).is_none(), "i={i} 不应触发");
        }
        let fired = st.step(&snap(13, 1200, vec![(CLASS_DOG, 0.8)]), TZ);
        assert!(fired.is_some(), "1200ms 应触发");
        assert_eq!(st.used_today, 1);
    }

    #[test]
    fn low_confidence_dog_never_triggers() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_tmp(&dir);
        let mut st = state(&cfg);
        // 0.6 < 0.65 → 3 秒都不触发
        for i in 0..30 {
            assert!(st.step(&snap(i + 1, i * 100, vec![(CLASS_DOG, 0.6)]), TZ).is_none());
        }
        assert_eq!(st.used_today, 0);
        assert!(st.last_action.is_empty(), "不应有配额用尽提示");
    }

    #[test]
    fn mean_above_threshold_though_some_low_frames() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_tmp(&dir);
        let mut st = state(&cfg);
        // 交替 0.95/0.5 → 均值 0.725 > 0.65 → 触发
        for i in 0..12 {
            let s = if i % 2 == 0 { 0.95 } else { 0.5 };
            assert!(st.step(&snap(i + 1, i * 100, vec![(CLASS_DOG, s)]), TZ).is_none());
        }
        assert!(st.step(&snap(13, 1200, vec![(CLASS_DOG, 0.95)]), TZ).is_some());
        assert_eq!(st.used_today, 1);
    }

    #[test]
    fn mean_below_threshold_never_triggers() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_tmp(&dir);
        let mut st = state(&cfg);
        // 交替 0.68/0.60 → 单帧 0.68 超阈值，但窗口均值 0.642 < 0.65 → 永不触发
        for i in 0..30 {
            let s = if i % 2 == 0 { 0.68 } else { 0.60 };
            assert!(st.step(&snap(i + 1, i * 100, vec![(CLASS_DOG, s)]), TZ).is_none());
        }
        assert_eq!(st.used_today, 0);
    }

    #[test]
    fn cat_present_resets_window() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_tmp(&dir);
        let mut st = state(&cfg);
        // 0..1100：狗窗口尚未满 1.2s，不触发（避免提前消耗配额）
        for i in 0..11 {
            st.step(&snap(i + 1, i * 100, vec![(CLASS_DOG, 0.8)]), TZ);
        }
        // 猫出现 → 窗口清空 + 之后 1.2s 内抑制
        assert!(st.step(&snap(16, 1500, vec![(CLASS_DOG, 0.8), (CLASS_CAT, 0.9)]), TZ).is_none());
        assert_eq!(st.dog_window.len(), 0, "猫应清空狗窗口");
        // 1600..2600 仍在猫抑制期（2600-1500=1100<1200），窗口保持清空
        for i in 0..11 {
            st.step(&snap(17 + i, 1600 + i * 100, vec![(CLASS_DOG, 0.8)]), TZ);
        }
        assert_eq!(st.dog_window.len(), 0, "抑制期内应保持无狗窗口");
        // 2700 起猫已出窗口（2700-1500=1200），重新积累
        for i in 0..12 {
            assert!(st.step(&snap(28 + i, 2700 + i * 100, vec![(CLASS_DOG, 0.8)]), TZ).is_none());
        }
        // 3900ms：窗口跨度 2700→3900=1200ms → 触发
        assert_eq!(st.step(&snap(40, 3900, vec![(CLASS_DOG, 0.8)]), TZ).is_some(), true);
    }

    #[test]
    fn quota_exhausted_no_trigger() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_tmp(&dir);
        let mut st = state(&cfg);
        st.used_today = 1; // 配额用尽（quota=1）
        st.last_quota_day = CtrlState::quota_slot_index(unix_now(), TZ, &[6]);
        for i in 0..21 {
            assert!(st.step(&snap(i + 1, i * 100, vec![(CLASS_DOG, 0.8)]), TZ).is_none());
        }
        assert_eq!(st.used_today, 1, "用尽后不得再消耗");
        assert!(st.last_action.contains("配额已用尽"));
    }

    #[test]
    fn quota_refreshes_at_hours() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_tmp(&dir);
        let mut st = state(&cfg);
        st.refresh_hours = vec![6, 18];
        st.used_today = 2;
        st.last_quota_day = 100; // 老槽位
        // 东八区 00:30（day=100, sec=30min, 未到任何整点 → n=0）
        let unix = 30 * 60 - TZ + 100 * 86400;
        assert_eq!(CtrlState::quota_slot_index(unix, TZ, &[6, 18]), 200);
        // 06:00 → n=1
        assert_eq!(CtrlState::quota_slot_index(unix + 5 * 3600 + 30 * 60, TZ, &[6, 18]), 201);
        // 18:00 → n=2
        assert_eq!(CtrlState::quota_slot_index(unix + 17 * 3600 + 30 * 60, TZ, &[6, 18]), 202);
        // 次日 00:30 → day=101, n=0 → 202（槽不变，不重复发放）
        assert_eq!(CtrlState::quota_slot_index(unix + 86400, TZ, &[6, 18]), 202);
        // 次日 06:00 → 203
        assert_eq!(CtrlState::quota_slot_index(unix + 86400 + 5 * 3600 + 30 * 60, TZ, &[6, 18]), 203);
        // refresh_if_due 用真实时钟: 当前槽 ≠ 100 则重置
        let cur = CtrlState::quota_slot_index(unix_now(), TZ, &[6, 18]);
        if cur != 100 {
            st.refresh_if_due(TZ);
            assert_eq!(st.used_today, 0);
        } else {
            st.refresh_if_due(TZ);
            assert_eq!(st.used_today, 2, "同槽内不重置");
        }
    }

    #[test]
    fn clock_invalid_blocks_refresh() {
        // RTC 上电默认 1970：此时槽位是假值，若照常刷新会误响一声并把 used_today 清零
        assert!(!CtrlState::clock_valid(0), "1970 未同步");
        assert!(!CtrlState::clock_valid(1_577_836_799), "2019-12-31 23:59:59 仍不可信");
        assert!(CtrlState::clock_valid(1_577_836_800), "2020-01-01 起可信");
        assert!(CtrlState::clock_valid(unix_now()), "当前真实时钟可信");
    }

    #[test]
    fn persist_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_tmp(&dir);
        let mut st = state(&cfg);
        st.used_today = 1;
        st.last_quota_day = 42;
        st.last_trigger_ms = 1234;
        st.quota = 5;
        st.socket_seconds = 20;
        st.refresh_hours = vec![9, 21];
        st.keep_video_on_pause = true;
        st.persist();
        let st2 = state(&cfg);
        assert_eq!(st2.used_today, 1);
        assert_eq!(st2.last_quota_day, 42);
        assert_eq!(st2.quota, 5, "配额应持久化");
        assert_eq!(st2.socket_seconds, 20, "通电时长应持久化");
        assert_eq!(st2.refresh_hours, vec![9, 21], "多刷新时刻应持久化");
        assert_eq!(st2.keep_video_on_pause, true, "保留视频流开关应持久化");
        assert_eq!(st2.dog_seconds, 1.2, "狗窗口固定 1.2");
        assert_eq!(st2.dog_min_score, 0.65, "置信阈值固定 0.65");
    }

    #[test]
    fn update_config_validates() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_tmp(&dir);
        let mut st = state(&cfg);
        // 合法值
        let v = st.update_config(&ConfigReq {
            quota: Some(4),
            socket_seconds: Some(30),
            refresh_hour: Some(7),
            ..Default::default()
        }).unwrap();
        assert_eq!(v["quota"], 4);
        assert_eq!(v["socket_seconds"], 30);
        assert!((v["dog_seconds"].as_f64().unwrap() - 1.2).abs() < 1e-3, "狗窗口不可改，恒为固定值");
        assert!((v["dog_min_score"].as_f64().unwrap() - 0.65).abs() < 1e-3, "置信阈值不可改，恒为固定值");
        assert_eq!(v["refresh_hours"], json!([7]), "旧 refresh_hour 应归一为 refresh_hours");
        // 多整点合法值（乱序+重复 → 排序去重）
        let v2 = st.update_config(&ConfigReq { refresh_hours: Some(vec![21, 6, 21, 18]), ..Default::default() }).unwrap();
        assert_eq!(v2["refresh_hours"], json!([6, 18, 21]));
        // 非法值拒绝
        assert!(st.update_config(&ConfigReq { quota: Some(0), ..Default::default() }).is_err());
        assert!(st.update_config(&ConfigReq { socket_seconds: Some(0), ..Default::default() }).is_err());
        assert!(st.update_config(&ConfigReq { refresh_hour: Some(24), ..Default::default() }).is_err());
        assert!(st.update_config(&ConfigReq { refresh_hours: Some(vec![]), ..Default::default() }).is_err());
        assert!(st.update_config(&ConfigReq { refresh_hours: Some(vec![24]), ..Default::default() }).is_err());
        // 拒绝后原值不变
        assert_eq!(st.quota, 4);
        assert_eq!(st.refresh_hours, vec![6, 18, 21]);
        // 持久化恢复
        let st2 = state(&cfg);
        assert_eq!(st2.quota, 4);
        assert_eq!(st2.refresh_hours, vec![6, 18, 21]);
    }

    #[test]
    fn gap_expires_window() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_tmp(&dir);
        let mut st = state(&cfg);
        for i in 0..10 {
            st.step(&snap(i + 1, i * 100, vec![(CLASS_DOG, 0.8)]), TZ);
        }
        // 断狗 2.2s（无狗帧）→ 窗口样本全部过期
        st.step(&snap(11, 3200, vec![]), TZ);
        assert_eq!(st.dog_window.len(), 0, "窗口应随样本过期清空");
        for i in 0..12 {
            st.step(&snap(12 + i, 3300 + i * 100, vec![(CLASS_DOG, 0.8)]), TZ);
        }
        assert_eq!(st.step(&snap(24, 4500, vec![(CLASS_DOG, 0.8)]), TZ).is_some(), true);
    }
}

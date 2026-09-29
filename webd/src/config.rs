//! 运行配置：全部支持环境变量注入（设备默认值 + 宿主测试覆盖）。

use std::net::SocketAddr;
use std::path::PathBuf;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct Config {
    /// 监听地址（默认 0.0.0.0:8080）
    pub listen: SocketAddr,
    /// HOME 目录：ewelink daemon socket + devicekey 缓存所在（默认 $HOME，设备端 /root）
    pub home: PathBuf,
    /// WiFi 标记文件目录（wificfgd 约定，默认 /boot）
    pub boot_dir: PathBuf,
    /// wxpush / mailpush 可执行文件
    pub wxpush_bin: PathBuf,
    pub mailpush_bin: PathBuf,
    /// 推送历史日志文件
    pub push_log: PathBuf,
    /// 系统日志文件（/var/log/messages）
    pub syslog: PathBuf,
    /// 微信/邮件凭证文件（仅健康检查读取，不输出内容）
    pub wxpusher_conf: PathBuf,
    pub mail_conf: PathBuf,
    /// ewelink 服务自启脚本（重启 daemon 用）
    pub ewelink_init: PathBuf,
    /// ewelink-rs CLI 路径 + 凭证文件（云端控制回退用）
    pub ewelink_bin: PathBuf,
    pub ewelink_env: PathBuf,
    /// 视觉调试模式：visiond 未运行时自造 SHM 数据供前端联调
    pub vision_debug: bool,
    /// 自动控制（喂狗）：窗口(control::DOG_WINDOW_S)内狗置信度均值 > control::DOG_MIN_SCORE 且无猫
    /// → 消耗配额 → 插座通电 + 邮件（窗口/阈值是代码内固定常量，不可 env/页面调整）
    pub ctrl_enabled: bool,
    pub ctrl_quota: u32,
    pub ctrl_refresh_hour: u32,
    pub ctrl_socket_seconds: u32,
    /// 配额用尽时是否保留实时视频流（true=保留画面只停推理；false=连摄像头/视频一起关）
    pub ctrl_keep_video_on_pause: bool,
    pub ctrl_socket_id: String,
    pub ctrl_socket_outlet: Option<u8>,
    pub ctrl_tz_offset: i64,
    pub ctrl_state_file: PathBuf,
    pub ctrl_shot_path: PathBuf,
}

fn env_path(key: &str, default: PathBuf) -> PathBuf {
    std::env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or(default)
}

impl Config {
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/root"));
        let listen = std::env::var("WEB_LISTEN")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| "0.0.0.0:8080".parse().unwrap());
        let vision_debug = std::env::var("WEB_VISION_DEBUG").is_ok();
        let ctrl_enabled = std::env::var("WEB_CTRL_ENABLED").map(|v| v != "0").unwrap_or(true);
        // 注意：狗窗口/置信度阈值不在此处解析——它们是 control.rs 里的固定常量
        // （DOG_WINDOW_S=1.2 / DOG_MIN_SCORE=0.65），WEB_CTRL_DOG_SECONDS / WEB_CTRL_DOG_MIN_SCORE 已废弃
        let ctrl_quota = std::env::var("WEB_CTRL_QUOTA").ok()
            .and_then(|s| s.parse().ok()).unwrap_or(1);
        let ctrl_refresh_hour = std::env::var("WEB_CTRL_REFRESH_HOUR").ok()
            .and_then(|s| s.parse::<u32>().ok()).unwrap_or(6).min(23);
        let ctrl_socket_seconds = std::env::var("WEB_CTRL_SOCKET_SECONDS").ok()
            .and_then(|s| s.parse().ok()).unwrap_or(10);
        let ctrl_keep_video_on_pause = std::env::var("WEB_CTRL_KEEP_VIDEO_ON_PAUSE")
            .map(|v| v != "0" && v != "false").unwrap_or(false);
        let ctrl_tz_offset = parse_tz_offset(&std::env::var("WEB_CTRL_TZ").unwrap_or_else(|_| "+0800".into()));
        Config {
            listen,
            home: home.clone(),
            boot_dir: env_path("WEB_BOOT_DIR", PathBuf::from("/boot")),
            wxpush_bin: env_path("WEB_WXPUSH_BIN", PathBuf::from("/mnt/system/usr/bin/wxpush")),
            mailpush_bin: env_path("WEB_MAILPUSH_BIN", PathBuf::from("/mnt/system/usr/bin/mailpush")),
            push_log: env_path("WEB_PUSH_LOG", PathBuf::from("/var/log/webd-push.log")),
            syslog: env_path("WEB_SYSLOG", PathBuf::from("/var/log/messages")),
            wxpusher_conf: env_path("WEB_WXPUSHER_CONF", PathBuf::from("/etc/wxpusher.env")),
            mail_conf: env_path("WEB_MAIL_CONF", PathBuf::from("/etc/mail.env")),
            ewelink_init: env_path("WEB_EWELINK_INIT", PathBuf::from("/etc/init.d/S90ewelink")),
            ewelink_bin: env_path("WEB_EWELINK_BIN", PathBuf::from("/mnt/system/usr/bin/ewelink-rs")),
            ewelink_env: env_path("WEB_EWELINK_ENV", PathBuf::from("/etc/ewelink.env")),
            vision_debug,
            ctrl_enabled,
            ctrl_quota,
            ctrl_refresh_hour,
            ctrl_socket_seconds,
            ctrl_keep_video_on_pause,
            ctrl_socket_id: std::env::var("WEB_CTRL_SOCKET_ID").unwrap_or_default(),
            // None = 全部插位（固件虚报多通道、物理仅一个插座时确保任何通道都被操作）
            ctrl_socket_outlet: std::env::var("WEB_CTRL_SOCKET_OUTLET").ok()
                .and_then(|s| s.parse::<u8>().ok()).map(|o| o.min(7)),
            ctrl_tz_offset,
            ctrl_state_file: env_path("WEB_CTRL_STATE", PathBuf::from("/mnt/data/webd_control.json")),
            ctrl_shot_path: env_path("WEB_CTRL_SHOT", PathBuf::from("/tmp/webd_ctrl_shot.jpg")),
        }
    }

    pub fn ewelink_socket(&self) -> PathBuf {
        self.home.join(".ewelink-rs-daemon.sock")
    }

    pub fn ewelink_keys(&self) -> PathBuf {
        self.home.join(".ewelink-rs-devicekeys.json")
    }

    pub fn ewelink_log(&self) -> PathBuf {
        self.home.join(".ewelink-rs-daemon.log")
    }
}

/// 解析时区偏移 "+0800"/"-0530" → 秒；非法返回 0
pub fn parse_tz_offset(s: &str) -> i64 {
    let s = s.trim();
    if s.len() != 5 || !s.starts_with(['+', '-']) {
        return 0;
    }
    let sign = if s.starts_with('-') { -1 } else { 1 };
    let hh: i64 = s[1..3].parse().unwrap_or(0);
    let mm: i64 = s[3..5].parse().unwrap_or(0);
    if hh > 14 || mm > 59 {
        return 0;
    }
    sign * (hh * 3600 + mm * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tz_offset_parsed() {
        assert_eq!(parse_tz_offset("+0800"), 8 * 3600);
        assert_eq!(parse_tz_offset("-0530"), -(5 * 3600 + 30 * 60));
        assert_eq!(parse_tz_offset("+0000"), 0);
        assert_eq!(parse_tz_offset("UTC"), 0);
        assert_eq!(parse_tz_offset("+9999"), 0);
    }
}

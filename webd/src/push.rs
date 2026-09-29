//! 推送集成：调用已部署的 wxpush / mailpush CLI，记录历史日志。
//! 退出码语义（两个工具对齐）：0 成功 / 1 凭证缺失 / 2 网络错误 / 3 服务端拒绝 / 4 参数错误。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::Config;

const EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const MAX_CONTENT: usize = 1000;

#[derive(Deserialize)]
pub struct WxPushReq {
    pub content: String,
}

#[derive(Deserialize)]
pub struct MailPushReq {
    pub content: String,
    pub title: Option<String>,
    pub image: Option<String>,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 凭证健康检查（只报存在性，不泄露内容）
pub async fn health(cfg: &Config) -> Value {
    let wx = credential_probe(&cfg.wxpusher_conf, &["WXPUSHER_SPT="]);
    let mail = credential_probe(&cfg.mail_conf, &["SMTP_HOST=", "SMTP_USER=", "MAIL_TO="]);
    json!({
        "wxpush": {
            "bin": cfg.wxpush_bin.exists(),
            "config": wx,
        },
        "mailpush": {
            "bin": cfg.mailpush_bin.exists(),
            "config": mail,
        },
    })
}

fn credential_probe(path: &Path, keys: &[&str]) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    json!({
        "exists": path.exists(),
        "keys_present": keys.iter().map(|k| text.lines().any(|l| l.trim_start().starts_with(k))).collect::<Vec<bool>>(),
        "keys": keys.iter().map(|k| k.trim_end_matches('=').to_string()).collect::<Vec<String>>(),
    })
}

/// 执行推送 CLI，写历史日志，返回 {exit, stdout, stderr}
async fn run_cli(cfg: &Config, bin: &std::path::Path, args: &[String], chan: &str, log_line: &str) -> Value {
    if !bin.exists() {
        return json!({"exit": -1, "error": format!("可执行文件不存在: {}", bin.display())});
    }
    let result = tokio::time::timeout(EXEC_TIMEOUT, tokio::process::Command::new(bin).args(args).output()).await;
    let (exit, stdout, stderr) = match result {
        Err(_) => (-1, String::new(), format!("执行超时（>{EXEC_TIMEOUT:?}）")),
        Ok(Err(e)) => (-1, String::new(), e.to_string()),
        Ok(Ok(o)) => (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        ),
    };
    // 历史记录：追加 JSON 行（截断 512KB）
    let entry = json!({
        "ts": now_ms(),
        "chan": chan,
        "args": args,
        "log": log_line,
        "exit": exit,
        "stdout": stdout,
        "stderr": stderr,
    });
    append_history(cfg, &entry.to_string()).await;
    json!({"exit": exit, "stdout": stdout, "stderr": stderr, "ts": now_ms()})
}

async fn append_history(cfg: &Config, line: &str) {
    if let Ok(mut f) = tokio::fs::OpenOptions::new().create(true).append(true).open(&cfg.push_log).await {
        let _ = tokio::io::AsyncWriteExt::write_all(&mut f, format!("{line}\n").as_bytes()).await;
    }
    // 超限截断（异步读改写）
    if let Ok(meta) = tokio::fs::metadata(&cfg.push_log).await {
        if meta.len() > 512 * 1024 {
            if let Ok(mut text) = tokio::fs::read_to_string(&cfg.push_log).await {
                let keep = text.split_off(text.len().saturating_sub(256 * 1024));
                let _ = tokio::fs::write(&cfg.push_log, keep).await;
            }
        }
    }
}

/// POST /api/push/wxpush {"content"}
pub async fn wxpush(cfg: &Config, req: WxPushReq) -> Value {
    let content = req.content.trim().to_string();
    if content.is_empty() || content.len() > MAX_CONTENT {
        return json!({"exit": 4, "error": format!("内容需为 1~{MAX_CONTENT} 字符")});
    }
    let args = vec![content.clone()];
    run_cli(cfg, &cfg.wxpush_bin, &args, "wxpush", &content).await
}

/// POST /api/push/mailpush {"content","title"?,"image"?}
pub async fn mailpush(cfg: &Config, req: MailPushReq) -> Value {
    let content = req.content.trim().to_string();
    if content.is_empty() || content.len() > MAX_CONTENT {
        return json!({"exit": 4, "error": format!("内容需为 1~{MAX_CONTENT} 字符")});
    }
    let mut args = vec![content.clone()];
    if let Some(t) = &req.title {
        if !t.trim().is_empty() {
            args.push("--title".into());
            args.push(t.trim().to_string());
        }
    }
    if let Some(img) = &req.image {
        args.push("--image".into());
        args.push(img.clone());
    }
    run_cli(cfg, &cfg.mailpush_bin, &args, "mailpush", &content).await
}

/// GET /api/push/history?lines=N → 最近 N 条历史（按行解析 JSON）
pub async fn history(cfg: &Config, lines: usize) -> Value {
    let lines = lines.clamp(1, 200);
    let text = tokio::fs::read_to_string(&cfg.push_log).await.unwrap_or_default();
    let mut entries: Vec<Value> = Vec::new();
    for line in text.lines().rev() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            entries.push(v);
        }
        if entries.len() >= lines {
            break;
        }
    }
    json!({"exists": cfg.push_log.exists(), "path": cfg.push_log.to_string_lossy(), "history": entries})
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn cfg_with_bins(dir: &TempDir, wx_exit: i32, mail_exit: i32) -> Config {
        let mut c = crate::config::Config::from_env();
        c.push_log = dir.path().join("push.log");
        c.wxpusher_conf = dir.path().join("wxpusher.env");
        c.mail_conf = dir.path().join("mail.env");
        let wx = dir.path().join("wxpush");
        let mail = dir.path().join("mailpush");
        std::fs::write(&wx, format!("#!/bin/sh\necho \"wx:$1\"\nexit {wx_exit}\n")).unwrap();
        std::fs::write(&mail, format!("#!/bin/sh\necho \"mail:$1\"\nexit {mail_exit}\n")).unwrap();
        std::fs::set_permissions(&wx, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&mail, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        c.wxpush_bin = wx;
        c.mailpush_bin = mail;
        c
    }

    #[tokio::test]
    async fn wxpush_success_records_history() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_with_bins(&dir, 0, 0);
        let v = wxpush(&cfg, WxPushReq { content: "测试".into() }).await;
        assert_eq!(v["exit"], 0);
        assert_eq!(v["stdout"], "wx:测试");
        let h = history(&cfg, 10).await;
        assert_eq!(h["history"].as_array().unwrap().len(), 1);
        assert_eq!(h["history"][0]["chan"], "wxpush");
        assert_eq!(h["history"][0]["exit"], 0);
    }

    #[tokio::test]
    async fn wxpush_validation() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_with_bins(&dir, 0, 0);
        let v = wxpush(&cfg, WxPushReq { content: "   ".into() }).await;
        assert_eq!(v["exit"], 4);
    }

    #[tokio::test]
    async fn mailpush_with_title() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_with_bins(&dir, 0, 0);
        let v = mailpush(&cfg, MailPushReq { content: "正文".into(), title: Some("主题".into()), image: None }).await;
        assert_eq!(v["exit"], 0, "full response: {v}");
        assert_eq!(v["stdout"], "mail:正文");
        let h = history(&cfg, 10).await;
        assert_eq!(h["history"][0]["args"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn missing_bin_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg_with_bins(&dir, 0, 0);
        cfg.wxpush_bin = dir.path().join("no-such-bin");
        let v = wxpush(&cfg, WxPushReq { content: "x".into() }).await;
        assert_eq!(v["exit"], -1);
        assert!(v["error"].as_str().unwrap().contains("不存在"));
    }
}
//! 日志查看：白名单日志源 + tail 最近 N 行（限制行数与总大小）。

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::config::Config;

/// 日志源白名单 → 实际路径（防止任意文件读取）
pub fn sources(cfg: &Config) -> HashMap<String, std::path::PathBuf> {
    let mut m = HashMap::new();
    m.insert("ewelink".to_string(), cfg.ewelink_log());
    m.insert("syslog".to_string(), cfg.syslog.clone());
    m.insert("push".to_string(), cfg.push_log.clone());
    m
}

pub const MAX_LINES: usize = 500;
pub const MAX_BYTES: usize = 128 * 1024;

/// tail：读取文件最后 lines 行（超 MAX_BYTES 截断）
pub async fn tail(path: &std::path::Path, lines: usize) -> Value {
    let lines = lines.clamp(1, MAX_LINES);
    let exists = path.exists();
    let mut content = String::new();
    if exists {
        content = tokio::fs::read_to_string(path).await.unwrap_or_default();
        if content.len() > MAX_BYTES {
            content = content.split_off(content.len() - MAX_BYTES);
        }
    }
    let all: Vec<&str> = content.lines().collect();
    let n = all.len().min(lines);
    let tail_lines: Vec<String> = all[all.len() - n..].iter().map(|s| s.to_string()).collect();
    json!({
        "name": path.file_name().and_then(|f| f.to_str()).unwrap_or(""),
        "path": path.to_string_lossy(),
        "exists": exists,
        "lines": tail_lines,
    })
}

/// 处理 GET /api/logs/{name}
pub async fn handler(cfg: &Config, name: &str, lines: usize) -> Result<Value, (axum::http::StatusCode, String)> {
    match sources(cfg).get(name) {
        Some(path) => Ok(tail(path, lines).await),
        None => Err((axum::http::StatusCode::NOT_FOUND, format!("未知日志源: {name}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_log(dir: &tempfile::TempDir) -> Config {
        let mut c = crate::config::Config::from_env();
        c.home = dir.path().to_path_buf();
        c.syslog = dir.path().join("messages");
        c.push_log = dir.path().join("push.log");
        c
    }

    #[tokio::test]
    async fn tail_picks_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_with_log(&dir);
        std::fs::write(dir.path().join("push.log"), "l1\nl2\nl3\nl4\nl5\n").unwrap();
        let v = tail(&cfg.push_log, 3).await;
        let lines: Vec<String> = v["lines"].as_array().unwrap().iter()
            .map(|x| x.as_str().unwrap().to_string()).collect();
        assert_eq!(lines, vec!["l3", "l4", "l5"]);
    }

    #[tokio::test]
    async fn missing_file_ok_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_with_log(&dir);
        let v = tail(&cfg.push_log, 10).await;
        assert_eq!(v["exists"], false);
        assert_eq!(v["lines"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn whitelist_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_with_log(&dir);
        assert!(handler(&cfg, "ewelink", 10).await.is_ok());
        assert!(handler(&cfg, "passwd", 10).await.is_err());
    }
}
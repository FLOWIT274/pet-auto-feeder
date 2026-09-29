use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法: wxpush <内容>");
        return ExitCode::from(4);
    }
    if args.iter().any(|a| a == "--image") {
        eprintln!("wxpush: --image 图片推送为 v2 功能，当前版本未实现");
        return ExitCode::from(4);
    }
    let content = &args[0];

    let spt = match load_spt() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("wxpush: {e}");
            return ExitCode::from(1);
        }
    };

    let base = std::env::var("WXPUSHER_BASE_URL")
        .unwrap_or_else(|_| "https://wxpusher.zjiecode.com".to_string());
    let url = format!("{base}/api/send/message/{spt}/{}", urlencode(content));

    match ureq::get(&url).call() {
        Ok(resp) => {
            let body = resp.into_body().read_to_string().unwrap_or_default();
            if body.contains("\"code\":1000") {
                println!("wxpush: 已发送");
                ExitCode::from(0)
            } else {
                eprintln!("wxpush: 服务端拒绝: {body}");
                ExitCode::from(3)
            }
        }
        Err(e) => {
            eprintln!("wxpush: 网络错误: {e}");
            ExitCode::from(2)
        }
    }
}

/// 从 WXPUSHER_CONFIG_DIR/wxpusher.env（默认 /etc）读取 WXPUSHER_SPT。
fn load_spt() -> Result<String, String> {
    let dir = std::env::var("WXPUSHER_CONFIG_DIR").unwrap_or_else(|_| "/etc".to_string());
    let path = std::path::Path::new(&dir).join("wxpusher.env");
    let text = std::fs::read_to_string(&path)
        .map_err(|_| format!("凭证文件不存在: {}", path.display()))?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("WXPUSHER_SPT=") {
            let v = v.trim();
            if !v.is_empty() {
                return Ok(v.to_string());
            }
        }
    }
    Err(format!("{} 中缺少 WXPUSHER_SPT", path.display()))
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

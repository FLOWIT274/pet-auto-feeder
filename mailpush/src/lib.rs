use base64::Engine;

/// 邮件配置（凭证加载结果）
#[derive(Debug, Clone)]
pub struct MailConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth_code: String,
    pub to: String,
}

/// 从 MAIL_CONFIG_DIR/mail.env（默认 /etc）加载配置。
pub fn load_config() -> Result<MailConfig, String> {
    let dir = std::env::var("MAIL_CONFIG_DIR").unwrap_or_else(|_| "/etc".to_string());
    let path = std::path::Path::new(&dir).join("mail.env");
    let text = std::fs::read_to_string(&path)
        .map_err(|_| format!("凭证文件不存在: {}", path.display()))?;

    let mut cfg = MailConfig {
        host: String::new(),
        port: 465,
        user: String::new(),
        auth_code: String::new(),
        to: String::new(),
    };
    for line in text.lines() {
        let line = line.trim();
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k {
            "SMTP_HOST" => cfg.host = v.to_string(),
            "SMTP_PORT" => cfg.port = v.parse().unwrap_or(465),
            "SMTP_USER" => cfg.user = v.to_string(),
            "SMTP_AUTH_CODE" => cfg.auth_code = v.to_string(),
            "MAIL_TO" => cfg.to = v.to_string(),
            _ => {}
        }
    }
    if cfg.host.is_empty() {
        return Err(format!("{} 中缺少 SMTP_HOST", path.display()));
    }
    if cfg.user.is_empty() {
        return Err(format!("{} 中缺少 SMTP_USER", path.display()));
    }
    if cfg.auth_code.is_empty() {
        return Err(format!("{} 中缺少 SMTP_AUTH_CODE", path.display()));
    }
    if cfg.to.is_empty() {
        return Err(format!("{} 中缺少 MAIL_TO", path.display()));
    }
    Ok(cfg)
}

/// 图片文件扩展名 → MIME 类型；不支持的返回 None。
pub fn image_mime(path: &std::path::Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        _ => None,
    }
}

/// RFC 2047 编码主题（=?UTF-8?B?...?=），纯 ASCII 原样返回。
pub fn encode_subject(subject: &str) -> String {
    if subject.is_ascii() {
        return subject.to_string();
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(subject.as_bytes());
    format!("=?UTF-8?B?{b64}?=")
}

/// 当前 UTC 时间，RFC 2822 格式（无 std 日期 API，用公历算法）。
pub fn rfc2822_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (h, min, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);

    // civil_from_days（Howard Hinnant 算法）
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    const WDAY: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTH: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let wday = (days + 4).rem_euclid(7) as usize;
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} +0000",
        WDAY[wday], d, MONTH[(m - 1) as usize], y, h, min, s
    )
}

/// 组装纯文本邮件（headers + 空行 + body，\r\n 行尾）。
pub fn build_mail_text(from: &str, to: &str, subject: &str, body: &str) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(&format!("From: <{from}>\r\n"));
    out.push_str(&format!("To: <{to}>\r\n"));
    out.push_str(&format!("Subject: {}\r\n", encode_subject(subject)));
    out.push_str(&format!("Date: {}\r\n", rfc2822_date()));
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    out.push_str("\r\n");
    out.push_str(body);
    out.into_bytes()
}

/// base64 编码并按 76 列折行。
pub fn base64_wrap(data: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
    let mut out = String::new();
    for chunk in b64.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(chunk).unwrap());
        out.push_str("\r\n");
    }
    out
}

/// HTML 转义正文（防 <>& 破坏邮件结构）。
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// 组装带内嵌图片的邮件（multipart/related + cid 引用，打开邮件直接显示图片）。
pub fn build_mail_image(
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
    image_bytes: &[u8],
    image_mime: &str,
    image_name: &str,
) -> Vec<u8> {
    let boundary = "mailpush-boundary-7f1c";
    let mut out = String::new();
    out.push_str(&format!("From: <{from}>\r\n"));
    out.push_str(&format!("To: <{to}>\r\n"));
    out.push_str(&format!("Subject: {}\r\n", encode_subject(subject)));
    out.push_str(&format!("Date: {}\r\n", rfc2822_date()));
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str(&format!(
        "Content-Type: multipart/related; boundary=\"{boundary}\"\r\n"
    ));
    out.push_str("\r\n");
    // 正文部分（HTML，cid 引用图片）
    out.push_str(&format!("--{boundary}\r\n"));
    out.push_str("Content-Type: text/html; charset=utf-8\r\n\r\n");
    out.push_str("<html><body><p>");
    out.push_str(&html_escape(body));
    out.push_str("</p><br/><img src=\"cid:img1\" style=\"max-width:100%\"/></body></html>");
    out.push_str("\r\n");
    // 图片部分（内嵌）
    out.push_str(&format!("--{boundary}\r\n"));
    out.push_str(&format!("Content-Type: {image_mime}; name=\"{image_name}\"\r\n"));
    out.push_str("Content-Transfer-Encoding: base64\r\n");
    out.push_str("Content-ID: <img1>\r\n");
    out.push_str(&format!(
        "Content-Disposition: inline; filename=\"{image_name}\"\r\n\r\n"
    ));
    out.push_str(&base64_wrap(image_bytes));
    out.push_str(&format!("--{boundary}--\r\n"));
    out.into_bytes()
}

/// SMTP 会话错误。
#[derive(Debug)]
pub enum SmtpError {
    /// 网络/IO 错误（exit 2）
    Network(String),
    /// 服务器拒绝（exit 3）
    Rejected { code: u16, line: String },
}

/// 读写抽象（生产=TLS 流，测试=明文 TcpStream）。
pub trait ReadWrite: std::io::Read + std::io::Write {}
impl<T: std::io::Read + std::io::Write> ReadWrite for T {}

/// 建立到 SMTP 服务器的连接：465 端口走 TLS，其他端口明文。
pub type Conn = Box<dyn ReadWrite>;
pub fn connect(cfg: &MailConfig) -> Result<Conn, SmtpError> {
    let tcp = std::net::TcpStream::connect((cfg.host.as_str(), cfg.port)).map_err(|e| {
        SmtpError::Network(format!("连接 {}:{} 失败: {e}", cfg.host, cfg.port))
    })?;
    if cfg.port != 465 {
        return Ok(Box::new(tcp));
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = std::sync::Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let name = rustls::pki_types::ServerName::try_from(cfg.host.clone())
        .map_err(|_| SmtpError::Network("非法的服务器名".to_string()))?;
    let conn = rustls::ClientConnection::new(config, name)
        .map_err(|e| SmtpError::Network(format!("TLS 初始化失败: {e}")))?;
    Ok(Box::new(rustls::StreamOwned::new(conn, tcp)))
}

/// 读取一个 SMTP 多行响应（`250-` 续行直到 `250 `），返回 (code, 完整文本)。
fn read_reply(conn: &mut Conn) -> Result<(u16, String), SmtpError> {
    let mut full = String::new();
    loop {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            conn.read_exact(&mut byte)
                .map_err(|e| SmtpError::Network(format!("读取响应失败: {e}")))?;
            if byte[0] == b'\n' {
                break;
            }
            line.push(byte[0]);
        }
        let s = std::str::from_utf8(&line).unwrap_or("").trim_end_matches('\r').to_string();
        full.push_str(&s);
        full.push('\n');
        if s.len() < 4 || s.as_bytes()[3] != b'-' {
            break;
        }
    }
    let code: u16 = full
        .split('\n')
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(3)
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    Ok((code, full))
}

/// 发送一行命令并期望指定响应码；不符返回 Rejected。
fn cmd_expect(conn: &mut Conn, cmd: &str, want: u16) -> Result<(), SmtpError> {
    conn.write_all(cmd.as_bytes())
        .map_err(|e| SmtpError::Network(format!("发送命令失败: {e}")))?;
    let (code, line) = read_reply(conn)?;
    if code != want {
        return Err(SmtpError::Rejected { code, line });
    }
    Ok(())
}

/// 执行 SMTP 会话：EHLO → AUTH LOGIN → MAIL → RCPT → DATA → QUIT。
pub fn smtp_send(conn: &mut Conn, cfg: &MailConfig, message: &[u8]) -> Result<(), SmtpError> {
    // 服务器先发 220 欢迎行
    let (code, line) = read_reply(conn)?;
    if code != 220 {
        return Err(SmtpError::Rejected { code, line });
    }

    cmd_expect(conn, "EHLO mailpush\r\n", 250)?;
    cmd_expect(conn, "AUTH LOGIN\r\n", 334)?;
    cmd_expect(
        conn,
        &format!("{}\r\n", base64::engine::general_purpose::STANDARD.encode(cfg.user.as_bytes())),
        334,
    )?;
    cmd_expect(
        conn,
        &format!("{}\r\n", base64::engine::general_purpose::STANDARD.encode(cfg.auth_code.as_bytes())),
        235,
    )?;
    cmd_expect(conn, &format!("MAIL FROM:<{}>\r\n", cfg.user), 250)?;
    cmd_expect(conn, &format!("RCPT TO:<{}>\r\n", cfg.to), 250)?;

    cmd_expect(conn, "DATA\r\n", 354)?;
    conn.write_all(message)
        .map_err(|e| SmtpError::Network(format!("发送正文失败: {e}")))?;
    cmd_expect(conn, "\r\n.\r\n", 250)?;

    conn.write_all(b"QUIT\r\n")
        .map_err(|e| SmtpError::Network(format!("发送 QUIT 失败: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_ascii_passthrough() {
        assert_eq!(encode_subject("hello"), "hello");
    }

    #[test]
    fn subject_chinese_encoded_rfc2047() {
        // "猫" UTF-8 = E7 8C AB → base64 = 54yr（python3 独立验证）
        assert_eq!(encode_subject("猫"), "=?UTF-8?B?54yr?=");
    }

    #[test]
    fn image_mime_by_extension() {
        assert_eq!(image_mime(std::path::Path::new("a.JPG")), Some("image/jpeg"));
        assert_eq!(image_mime(std::path::Path::new("b.png")), Some("image/png"));
        assert_eq!(image_mime(std::path::Path::new("c.gif")), None);
        assert_eq!(image_mime(std::path::Path::new("noext")), None);
    }

    #[test]
    fn text_mail_has_core_headers() {
        let m = build_mail_text("u@q.com", "u@q.com", "hi", "你好\n猫");
        let s = String::from_utf8(m).unwrap();
        assert!(s.starts_with("From: <u@q.com>\r\n"));
        assert!(s.contains("\r\nTo: <u@q.com>\r\n"));
        assert!(s.contains("\r\nSubject: hi\r\n"));
        assert!(s.contains("\r\nContent-Type: text/plain; charset=utf-8\r\n"));
        assert!(s.contains("\r\n\r\n你好\n猫"), "正文应原样保留");
        assert!(s.contains("\r\n"), "行尾应为 CRLF");
        assert!(!s.contains("\n\n"), "不应有裸 LF 空行");
    }

    #[test]
    fn image_mail_is_inline_embedded() {
        let m = build_mail_image("u@q.com", "u@q.com", "pic", "看<图>", &[0xFF, 0xD8, 0xFF], "image/jpeg", "a.jpg");
        let s = String::from_utf8(m).unwrap();
        assert!(s.contains("multipart/related"), "内嵌应为 multipart/related");
        assert!(s.contains("<img src=\"cid:img1\""), "正文应引用 cid");
        assert!(s.contains("Content-ID: <img1>"), "图片部分应有 Content-ID");
        assert!(s.contains("Content-Disposition: inline"), "应为 inline 而非 attachment");
        assert!(s.contains("看&lt;图&gt;"), "正文应 HTML 转义: {s}");
        // [0xFF,0xD8,0xFF] base64 = /9j/（python 独立验证）
        assert!(s.contains("/9j/"));
        assert!(s.ends_with("--mailpush-boundary-7f1c--\r\n"));
    }
}

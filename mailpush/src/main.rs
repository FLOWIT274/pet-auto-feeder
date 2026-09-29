use std::process::ExitCode;

fn main() -> ExitCode {
    // 参数解析：<内容> [--title <主题>] [--image <图片文件>]
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法: mailpush <内容> [--title <主题>] [--image <图片文件>]");
        return ExitCode::from(4);
    }
    let mut content: Option<&str> = None;
    let mut title = String::new();
    let mut image: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--title" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("mailpush: --title 缺少值");
                    return ExitCode::from(4);
                }
                title = args[i].clone();
            }
            "--image" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("mailpush: --image 缺少值");
                    return ExitCode::from(4);
                }
                image = Some(args[i].as_str());
            }
            other => {
                if content.is_none() {
                    content = Some(other);
                } else {
                    eprintln!("mailpush: 无法识别的参数: {other}");
                    return ExitCode::from(4);
                }
            }
        }
        i += 1;
    }
    let content = match content {
        Some(c) => c,
        None => {
            eprintln!("用法: mailpush <内容> [--title <主题>] [--image <图片文件>]");
            return ExitCode::from(4);
        }
    };

    let cfg = match mailpush::load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mailpush: {e}");
            return ExitCode::from(1);
        }
    };

    let message = if let Some(path) = image {
        let p = std::path::Path::new(path);
        let mime = match mailpush::image_mime(p) {
            Some(m) => m,
            None => {
                eprintln!("mailpush: 不支持的图片格式（仅 jpg/jpeg/png）: {path}");
                return ExitCode::from(4);
            }
        };
        let bytes = match std::fs::read(p) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("mailpush: 图片文件读取失败: {e}");
                return ExitCode::from(4);
            }
        };
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("image");
        mailpush::build_mail_image(&cfg.user, &cfg.to, &title, content, &bytes, mime, name)
    } else {
        mailpush::build_mail_text(&cfg.user, &cfg.to, &title, content)
    };

    let mut conn = match mailpush::connect(&cfg) {
        Ok(c) => c,
        Err(mailpush::SmtpError::Network(e)) => {
            eprintln!("mailpush: 网络错误: {e}");
            return ExitCode::from(2);
        }
        Err(other) => {
            eprintln!("mailpush: 连接失败: {other:?}");
            return ExitCode::from(3);
        }
    };

    match mailpush::smtp_send(&mut conn, &cfg, &message) {
        Ok(()) => {
            println!("mailpush: 已发送");
            ExitCode::from(0)
        }
        Err(mailpush::SmtpError::Network(e)) => {
            eprintln!("mailpush: 网络错误: {e}");
            ExitCode::from(2)
        }
        Err(mailpush::SmtpError::Rejected { code, line }) => {
            eprintln!("mailpush: 服务器拒绝 ({code}): {}", line.trim());
            ExitCode::from(3)
        }
    }
}

use std::process::Command;

mod common;

/// 用注入的 MAIL_CONFIG_DIR 运行 mailpush，返回 (exit_code, stdout, stderr)
fn run_with(config_dir: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_mailpush"))
        .args(args)
        .env("MAIL_CONFIG_DIR", config_dir)
        .output()
        .expect("运行 mailpush 失败");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "mailpush-test-{tag}-{}",
        std::process::id()
    ))
}

fn write_config(dir: &std::path::Path, port: u16) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("mail.env"),
        format!(
            "SMTP_HOST=127.0.0.1\nSMTP_PORT={port}\nSMTP_USER=user@qq.com\nSMTP_AUTH_CODE=authcode123\nMAIL_TO=user@qq.com\n"
        ),
    )
    .unwrap();
}

#[test]
fn no_args_exits_4() {
    let dir = tmp_dir("noargs");
    std::fs::create_dir_all(&dir).unwrap();
    let (code, _, err) = run_with(&dir, &[]);
    assert_eq!(code, 4, "无参数应退出码 4: {err}");
    assert!(err.contains("用法"), "应打印用法: {err}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_dir_missing_exits_1() {
    let dir = tmp_dir("missing").join("nonexistent");
    let (code, _, err) = run_with(&dir, &["hello"]);
    assert_eq!(code, 1, "凭证目录不存在应退出码 1: {err}");
    assert!(err.contains("凭证"), "应提示凭证问题: {err}");
}

#[test]
fn config_missing_field_exits_1() {
    let dir = tmp_dir("field");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mail.env"), "SMTP_HOST=smtp.qq.com\n").unwrap();
    let (code, _, err) = run_with(&dir, &["hello"]);
    assert_eq!(code, 1, "凭证缺字段应退出码 1: {err}");
    assert!(err.contains("SMTP_USER"), "应指出缺失字段: {err}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn image_unsupported_extension_exits_4() {
    let dir = tmp_dir("gif");
    write_config(&dir, 465);
    let img = dir.join("a.gif");
    std::fs::write(&img, b"GIF89a").unwrap();
    let (code, _, err) = run_with(&dir, &["hi", "--image", img.to_str().unwrap()]);
    assert_eq!(code, 4, "gif 附件应退出码 4: {err}");
    assert!(err.contains("图片"), "应提示图片格式: {err}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn image_file_missing_exits_4() {
    let dir = tmp_dir("nofile");
    write_config(&dir, 465);
    let img = dir.join("missing.jpg");
    let (code, _, err) = run_with(&dir, &["hi", "--image", img.to_str().unwrap()]);
    assert_eq!(code, 4, "图片文件不存在应退出码 4: {err}");
    assert!(err.contains("图片"), "应提示图片问题: {err}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn network_error_exits_2() {
    // 端口 1 无监听（几乎必然连接失败）
    let dir = tmp_dir("net");
    write_config(&dir, 1);
    let (code, _, err) = run_with(&dir, &["hi"]);
    assert_eq!(code, 2, "网络错误应退出码 2: {err}");
    assert!(err.contains("网络"), "应提示网络错误: {err}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn smtp_rejected_exits_3() {
    let (port, _rx) = common::spawn_mock(&[
        "220 mock ESMTP ready\r\n",
        "250-mock\r\n250 AUTH LOGIN PLAIN\r\n",
        "334 VXNlcm5hbWU6\r\n",
        "334 UGFzc3dvcmQ6\r\n",
        "535 5.7.8 Authentication credentials invalid\r\n",
    ]);
    let dir = tmp_dir("rej");
    write_config(&dir, port);
    let (code, _, err) = run_with(&dir, &["hi"]);
    assert_eq!(code, 3, "SMTP 拒绝应退出码 3: {err}");
    assert!(err.contains("拒绝"), "应提示服务器拒绝: {err}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn full_push_exits_0_and_sends_mail() {
    let (port, rx) = common::spawn_mock(&[
        "220 mock ESMTP ready\r\n",
        "250-mock\r\n250 AUTH LOGIN PLAIN\r\n",
        "334 VXNlcm5hbWU6\r\n",
        "334 UGFzc3dvcmQ6\r\n",
        "235 2.7.0 Authentication successful\r\n",
        "250 2.1.0 Ok\r\n",
        "250 2.1.5 Ok\r\n",
        "354 End data with <CR><LF>.<CR><LF>\r\n",
        "250 2.0.0 Ok: queued\r\n",
        "221 2.0.0 Bye\r\n",
    ]);
    let dir = tmp_dir("full");
    write_config(&dir, port);
    let img = dir.join("cat.jpg");
    std::fs::write(&img, vec![0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
    let (code, out, err) = run_with(
        &dir,
        &["猫来了", "--title", "猫狗通知", "--image", img.to_str().unwrap()],
    );
    assert_eq!(code, 0, "完整推送应成功: {err}");
    assert!(out.contains("已发送"), "应输出成功: {out}");

    let log = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(log.contains("Subject: =?UTF-8?B?54yr54uX6YCa55+l?="), "标题应 RFC2047 编码");
    assert!(log.contains("multipart/related"), "应包含内嵌图片");
    assert!(log.contains("cid:img1"), "应引用 cid");
    assert!(log.contains("/9j/4A=="), "图片 base64 应出现在 DATA");
    assert!(log.contains("QUIT\r\n"));
    std::fs::remove_dir_all(&dir).ok();
}

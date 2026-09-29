mod common;

use mailpush::{connect, smtp_send, MailConfig};

fn spawn_mock(script: &[&str]) -> (u16, std::sync::mpsc::Receiver<String>) {
    common::spawn_mock(script)
}

fn cfg_with_port(port: u16) -> MailConfig {
    common::cfg_with_port(port)
}

#[test]
fn full_smtp_session_sends_expected_commands() {
    // 脚本化响应序列（SMTP 官方代码）
    let (port, rx) = spawn_mock(&[
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

    let cfg = cfg_with_port(port);
    let mut conn = connect(&cfg).expect("连接 mock 服务器");
    let msg = mailpush::build_mail_text("user@qq.com", "user@qq.com", "测试", "你好猫");
    smtp_send(&mut conn, &cfg, &msg).expect("SMTP 会话应成功");

    let log = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("mock 应返回日志");
    // 命令序列验证（顺序敏感）
    let cmds: [&str; 11] = [
        "EHLO ", "AUTH LOGIN\r\n", "dXNlckBxcS5jb20=", // base64("user@qq.com")
        "YXV0aGNvZGUxMjM=",                            // base64("authcode123")
        "MAIL FROM:<user@qq.com>\r\n",
        "RCPT TO:<user@qq.com>\r\n",
        "DATA\r\n",
        "Subject: =?UTF-8?B?5rWL6K+V?=\r\n", // "测试" base64
        "你好猫",
        "\r\n.\r\n",
        "QUIT\r\n",
    ];
    let mut pos = 0;
    for c in cmds {
        let found = log[pos..].find(c);
        assert!(found.is_some(), "日志缺少命令 {c:?}\n完整日志:\n{log}");
        pos += found.unwrap() + c.len();
    }
}

#[test]
fn auth_rejected_exits_rejected_error() {
    let (port, _rx) = spawn_mock(&[
        "220 mock ESMTP ready\r\n",
        "250-mock\r\n250 AUTH LOGIN PLAIN\r\n",
        "334 VXNlcm5hbWU6\r\n",
        "334 UGFzc3dvcmQ6\r\n",
        "535 5.7.8 Authentication credentials invalid\r\n",
    ]);
    let cfg = cfg_with_port(port);
    let mut conn = connect(&cfg).unwrap();
    let msg = mailpush::build_mail_text("user@qq.com", "user@qq.com", "t", "b");
    let err = smtp_send(&mut conn, &cfg, &msg).unwrap_err();
    match err {
        mailpush::SmtpError::Rejected { code, .. } => assert_eq!(code, 535),
        other => panic!("期望 Rejected(535)，实际 {other:?}"),
    }
}

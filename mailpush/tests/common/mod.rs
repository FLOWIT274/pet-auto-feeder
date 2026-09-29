use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;

use mailpush::MailConfig;

/// 脚本化 SMTP mock 服务器：accept 一个连接，先发 220 欢迎行，
/// 然后按脚本逐条响应命令，把所有收到的原始字节经 mpsc 发回测试线程。
/// 收到 QUIT 后按 SMTP 语义关闭连接并发送日志。
pub fn spawn_mock(script: &[&str]) -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    let script: Vec<String> = script.iter().map(|s| s.to_string()).collect();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        sock.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut reader = BufReader::new(sock.try_clone().unwrap());
        let mut log = String::new();
        let mut script_i = 0usize;
        // SMTP 服务器先发 220 欢迎行
        let greeting = script.get(0).cloned().unwrap_or_else(|| "220 mock ESMTP\r\n".into());
        sock.write_all(greeting.as_bytes()).unwrap();
        sock.flush().unwrap();
        script_i += 1;
        let mut in_data = false;
        let mut data_buf: Vec<u8> = Vec::new();
        loop {
            if in_data {
                // 逐行读，检测结束符 "\r\n.\r\n"
                let mut line = Vec::new();
                let n = reader.read_until(b'\n', &mut line).unwrap();
                if n == 0 {
                    break;
                }
                data_buf.extend_from_slice(&line);
                log.push_str(&String::from_utf8_lossy(&line));
                if line == b".\r\n" {
                    in_data = false;
                    let line = script.get(script_i).cloned().unwrap_or_else(|| "250 ok".into());
                    sock.write_all(line.as_bytes()).unwrap();
                    sock.flush().unwrap();
                    script_i += 1;
                }
                continue;
            }
            let mut line = Vec::new();
            let n = reader.read_until(b'\n', &mut line).unwrap();
            if n == 0 {
                break;
            }
            let line_str = String::from_utf8_lossy(&line).to_string();
            log.push_str(&line_str);
            if line_str.starts_with("DATA") {
                in_data = true;
                let resp = script.get(script_i).cloned().unwrap_or_else(|| "354 go".into());
                sock.write_all(resp.as_bytes()).unwrap();
                sock.flush().unwrap();
                script_i += 1;
                continue;
            }
            let resp = script.get(script_i).cloned().unwrap_or_else(|| "250 ok".into());
            sock.write_all(resp.as_bytes()).unwrap();
            sock.flush().unwrap();
            script_i += 1;
            if line_str.starts_with("QUIT") {
                break; // SMTP 语义：QUIT 后服务器关闭连接
            }
        }
        tx.send(log).ok();
    });
    (port, rx)
}

pub fn cfg_with_port(port: u16) -> MailConfig {
    MailConfig {
        host: "127.0.0.1".to_string(),
        port,
        user: "user@qq.com".to_string(),
        auth_code: "authcode123".to_string(),
        to: "user@qq.com".to_string(),
    }
}

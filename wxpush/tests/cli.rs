use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

/// 启动一个 mock HTTP server，接收一次请求并返回指定 body。
/// 返回 (base_url, 请求文本接收端)。
fn start_mock_with(body: &str) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    let body = body.to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap();
        tx.send(String::from_utf8_lossy(&buf[..n]).to_string()).unwrap();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });
    (format!("http://{}", addr), rx)
}

/// 启动一个 mock HTTP server，接收一次请求并返回 WxPusher 成功响应。
/// 返回 (base_url, 请求文本接收端)。
fn start_mock() -> (String, mpsc::Receiver<String>) {
    start_mock_with(r#"{"code":1000,"msg":"处理成功","data":[{"code":1000,"status":"ok"}]}"#)
}

/// 创建临时配置目录，写入 wxpusher.env（含 SPT）。目录名带 tag 避免并行测试互删。
fn make_config_tagged(tag: &str, spt: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wxpush-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("wxpusher.env"), format!("WXPUSHER_SPT={spt}\n")).unwrap();
    dir
}

fn make_config(spt: &str) -> std::path::PathBuf {
    make_config_tagged("cfg", spt)
}

fn run_wxpush(config_dir: &std::path::Path, base_url: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_wxpush"))
        .args(args)
        .env("WXPUSHER_CONFIG_DIR", config_dir)
        .env("WXPUSHER_BASE_URL", base_url)
        .output()
        .expect("failed to spawn wxpush")
}

#[test]
fn push_text_success_exit0() {
    let (base, rx) = start_mock();
    let dir = make_config_tagged("push", "SPT_test");
    let out = run_wxpush(&dir, &base, &["你好猫"]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let req = rx.recv_timeout(Duration::from_secs(2)).expect("mock server got no request");
    let req_line = req.lines().next().unwrap();
    // SPT 极简 GET：/api/send/message/{SPT}/{内容}，内容需 URL 编码
    assert!(
        req_line.starts_with("GET /api/send/message/SPT_test/"),
        "unexpected request line: {req_line}"
    );
    assert!(
        req_line.contains("%E4%BD%A0%E5%A5%BD%E7%8C%AB"),
        "content not URL-encoded in: {req_line}"
    );
}

#[test]
fn missing_credentials_exit1() {
    // 配置目录存在、env 文件存在，但无 WXPUSHER_SPT 行
    let dir = std::env::temp_dir().join(format!("wxpush-missingcred-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("wxpusher.env"), "# 空配置\n").unwrap();
    let out = run_wxpush(&dir, "http://127.0.0.1:1", &["内容"]);

    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("WXPUSHER_SPT"), "stderr: {stderr}");
}

#[test]
fn missing_env_file_exit1() {
    // 配置目录根本不存在
    let dir = std::env::temp_dir().join(format!("wxpush-noenv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let out = run_wxpush(&dir, "http://127.0.0.1:1", &["内容"]);

    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("凭证文件不存在"), "stderr: {stderr}");
}

#[test]
fn server_rejects_exit3() {
    // 服务端返回业务失败（code != 1000）
    let (base, _rx) = start_mock_with(r#"{"code":4001,"msg":"SPT 无效"}"#);
    let dir = make_config_tagged("reject", "SPT_bad");
    let out = run_wxpush(&dir, &base, &["内容"]);

    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("服务端拒绝"), "stderr: {stderr}");
}

#[test]
fn network_error_exit2() {
    // 指向一个没有监听者的端口 → 连接拒绝
    let dir = make_config_tagged("neterr", "SPT_test");
    let out = run_wxpush(&dir, "http://127.0.0.1:1", &["内容"]);

    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("网络错误"), "stderr: {stderr}");
}

#[test]
fn image_flag_not_implemented_exit4() {
    // v1 未实现图片推送，--image 必须 fail-fast 而非静默忽略
    let (base, _rx) = start_mock();
    let dir = make_config_tagged("img", "SPT_test");
    let out = run_wxpush(&dir, &base, &["内容", "--image", "/tmp/x.jpg"]);

    assert_eq!(out.status.code(), Some(4));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--image"), "stderr: {stderr}");
    assert!(stderr.contains("v2"), "stderr: {stderr}");
}

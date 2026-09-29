//! 分步诊断：TCP -> TLS -> HTTP Upgrade，支持直连 / 代理隧道两种模式
//! 用法: cargo run --example tls_probe -- <domain> [direct|proxy]
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_native_tls::TlsConnector;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let domain = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "cn-pconnect2.coolkit.cc".to_string());
    let mode = std::env::args().nth(2).unwrap_or_else(|| "direct".to_string());
    let port: u16 = 443;

    // 1) TCP：直连 pconnect 或连代理并 CONNECT 隧道
    let (tcp, label) = if mode == "proxy" {
        let tcp = tokio::time::timeout(
            Duration::from_secs(8),
            TcpStream::connect(("127.0.0.1", 7897)),
        )
        .await;
        let mut tcp = match tcp {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                println!("[1] 代理 TCP connect ERR: {e}");
                return Ok(());
            }
            Err(_) => {
                println!("[1] 代理 TCP connect TIMEOUT");
                return Ok(());
            }
        };
        let req = format!(
            "CONNECT {domain}:{port} HTTP/1.1\r\nHost: {domain}:{port}\r\n\r\n"
        );
        tcp.write_all(req.as_bytes()).await?;
        tcp.flush().await?;
        let mut buf = [0u8; 1024];
        let n = tokio::time::timeout(Duration::from_secs(8), tcp.read(&mut buf))
            .await
            .map_err(|_| anyhow::anyhow!("CONNECT 响应超时"))??;
        let resp = String::from_utf8_lossy(&buf[..n]);
        println!("[1] CONNECT 响应: {}", resp.lines().next().unwrap_or(""));
        if !resp.starts_with("HTTP/1.1 200") {
            return Ok(());
        }
        (tcp, format!("{domain}:{port} via 代理"))
    } else {
        let tcp = tokio::time::timeout(
            Duration::from_secs(8),
            TcpStream::connect((domain.as_str(), port)),
        )
        .await;
        match tcp {
            Ok(Ok(s)) => (s, format!("{domain}:{port} 直连")),
            Ok(Err(e)) => {
                println!("[1] TCP connect ERR: {e}");
                return Ok(());
            }
            Err(_) => {
                println!("[1] TCP connect TIMEOUT");
                return Ok(());
            }
        }
    };
    println!("[1] TCP OK ({label})");

    // 2) TLS（跳过证书验证，聚焦握手可达性）
    let tls = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .build()?;
    let tls = TlsConnector::from(tls);
    let tlsfut = tls.connect(&domain, tcp);
    let tlsres = tokio::time::timeout(Duration::from_secs(8), tlsfut).await;
    let mut stream = match tlsres {
        Ok(Ok(s)) => {
            println!("[2] TLS handshake OK");
            s
        }
        Ok(Err(e)) => {
            println!("[2] TLS handshake ERR: {e}");
            return Ok(());
        }
        Err(_) => {
            println!("[2] TLS handshake TIMEOUT");
            return Ok(());
        }
    };

    // 3) HTTP Upgrade
    let req = format!(
        "GET /api/ws HTTP/1.1\r\nHost: {domain}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;
    let mut buf = [0u8; 4096];
    let r = tokio::time::timeout(Duration::from_secs(8), stream.read(&mut buf)).await;
    match r {
        Ok(Ok(n)) => println!(
            "[3] Upgrade 响应 {} 字节: {}",
            n,
            String::from_utf8_lossy(&buf[..n.min(300)])
        ),
        Ok(Err(e)) => println!("[3] Upgrade read ERR: {e}"),
        Err(_) => println!("[3] Upgrade TIMEOUT (无响应)"),
    }
    Ok(())
}
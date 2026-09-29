//! eWeLink WebSocket 长连接
//!
//! 官方推荐使用长连接下发控制与被动接收设备状态变化：
//! 1. GET https://{region}-dispa.coolkit.xx/dispatch/app 获取长连接服务器 domain:port
//! 2. 连接 wss://{domain}:{port}/api/ws
//! 3. 发送 userOnline 握手（at / apikey / appid / nonce / sequence / version=8）
//! 4. 发送 update（控制）/ query（查询）指令；服务端推送 action=update 的设备状态变化
//! 5. 按 config.hbInterval 心跳保活

use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use native_tls::TlsConnector as NativeTlsConnector;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_native_tls::TlsConnector as TokioTlsConnector;
use tokio_native_tls::TlsStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::api::Client;
use crate::sign::{nonce, now_secs, sequence};

type WsStream = WebSocketStream<TlsStream<TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsRead = futures_util::stream::SplitStream<WsStream>;

/// 长连接分配服务地址（《接口中心_v2 - 分配服务》）
fn dispatch_url(region: &str) -> &'static str {
    match region {
        "cn" => "https://cn-dispa.coolkit.cn/dispatch/app",
        "as" => "https://as-dispa.coolkit.cc/dispatch/app",
        "us" => "https://us-dispa.coolkit.cc/dispatch/app",
        "eu" => "https://eu-dispa.coolkit.cc/dispatch/app",
        _ => "https://as-dispa.coolkit.cc/dispatch/app",
    }
}

/// 请求分配服务，拿到 (domain, port)
async fn dispatch(region: &str) -> Result<(String, u16)> {
    let text = reqwest::get(dispatch_url(region))
        .await?
        .text()
        .await?;
    let v: Value = serde_json::from_str(&text)
        .with_context(|| format!("解析分配服务响应失败: {text}"))?;
    if v["error"].as_i64() != Some(0) {
        bail!("分配服务失败: {text}");
    }
    let domain = v["domain"].as_str().context("分配响应缺少 domain")?.to_string();
    let port = v["port"].as_u64().context("分配响应缺少 port")? as u16;
    Ok((domain, port))
}

/// 解析代理地址（http://host:port / host:port），默认端口 8080
fn parse_proxy(s: &str) -> Option<(String, u16)> {
    let s = s.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    let s = s
        .strip_prefix("http://")
        .or_else(|| s.strip_prefix("https://"))
        .unwrap_or(s);
    match s.rsplit_once(':') {
        Some((h, p)) => p
            .parse()
            .ok()
            .filter(|_| !h.is_empty())
            .map(|port| (h.to_string(), port)),
        None => Some((s.to_string(), 8080)),
    }
}

/// 连接 wss 并完成 userOnline 握手，返回 (发送端, 接收端)
async fn connect(appid: &str, at: &str, apikey: &str, region: &str) -> Result<(WsSink, WsRead, Value)> {
    let (domain, port) = dispatch(region).await?;
    let url = format!("wss://{domain}:{port}/api/ws");

    // 设置 HTTPS_PROXY / https_proxy / ALL_PROXY 时经 HTTP CONNECT 隧道连接，
    // 否则直连。统一在 TLS 完成后再做 WebSocket 升级。
    let tls = NativeTlsConnector::builder()
        .build()
        .context("初始化 TLS 失败")?;
    let tls = TokioTlsConnector::from(tls);

    let proxy = std::env::var("HTTPS_PROXY")
        .ok()
        .or_else(|| std::env::var("https_proxy").ok())
        .or_else(|| std::env::var("ALL_PROXY").ok())
        .and_then(|s| parse_proxy(&s));

    let stream: TlsStream<TcpStream> = match &proxy {
        Some((ph, pp)) => {
            let mut tcp = TcpStream::connect((ph.as_str(), *pp))
                .await
                .with_context(|| format!("连接代理 {ph}:{pp} 失败"))?;
            let req = format!("CONNECT {domain}:{port} HTTP/1.1\r\nHost: {domain}:{port}\r\n\r\n");
            tcp.write_all(req.as_bytes()).await?;
            tcp.flush().await?;
            // 读取 CONNECT 响应头直到空行
            let mut buf = Vec::with_capacity(1024);
            let mut tmp = [0u8; 1024];
            loop {
                let n = tcp.read(&mut tmp).await?;
                if n == 0 {
                    bail!("代理 CONNECT 连接被关闭");
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&buf);
            let code = head.split_whitespace().nth(1).unwrap_or("");
            if code != "200" {
                bail!("代理 CONNECT 失败: {head}");
            }
            println!("(已通过代理 {ph}:{pp} 连接)");
            tls.connect(&domain, tcp)
                .await
                .context("代理隧道内 TLS 握手失败")?
        }
        None => {
            let tcp = TcpStream::connect((domain.as_str(), port))
                .await
                .with_context(|| format!("连接长连接服务器失败: {domain}:{port}"))?;
            tls.connect(&domain, tcp)
                .await
                .with_context(|| {
                    format!("TLS 握手失败: {domain}:{port}（若持续超时，可设置 HTTPS_PROXY 走代理）")
                })?
        }
    };
    println!("连接长连接服务器: {url}");

    let (ws, _resp) = tokio_tungstenite::client_async(&url, stream)
        .await
        .with_context(|| format!("WebSocket 握手失败: {url}"))?;
    let (mut write, mut read) = ws.split();

    let handshake = json!({
        "action": "userOnline",
        "version": 8,
        "ts": now_secs(),
        "at": at,
        "userAgent": "app",
        "apikey": apikey,
        "appid": appid,
        "nonce": nonce(),
        "sequence": sequence(),
    });
    write
        .send(Message::Text(handshake.to_string()))
        .await?;
    println!("userOnline 握手已发送");

    // 等待握手响应（约 3 秒），返回服务端 config（含心跳间隔）
    let deadline = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(deadline);
    let ack = loop {
        tokio::select! {
            msg = read.next() => match msg {
                Some(Ok(Message::Text(t))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if v.get("error").and_then(|e| e.as_i64()) == Some(0)
                            && v.get("sequence").is_some() {
                            println!("握手成功: {v}");
                            break v;
                        }
                        println!("<< {v}");
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => bail!("握手期间 websocket 错误: {e}"),
                None => bail!("连接被关闭"),
            },
            _ = &mut deadline => bail!("握手超时"),
        }
    };
    Ok((write, read, ack))
}

/// 建立长连接并持续监听设备状态。
/// 若传入 deviceid + "on"/"off"，会在连接建立 2 秒后演示通过 WS 下发控制指令。
pub async fn run(client: &Client, deviceid: Option<&str>, switch: Option<&str>) -> Result<()> {
    let appid = client.config.appid.clone();
    let at = client.at.as_deref().context("请先登录（先执行 login）")?;
    let apikey = client.apikey.as_deref().context("请先登录（先执行 login）")?;
    let region = client.config.region.clone();

    let (mut write, mut read, handshake) = connect(&appid, at, apikey, &region).await?;

    // 心跳间隔：优先用服务端 config.hbInterval，默认 60s
    let hb_interval = handshake
        .get("config")
        .and_then(|c| c.get("hbInterval"))
        .and_then(|v| v.as_u64())
        .map(|s| Duration::from_secs(s.saturating_sub(7)))
        .unwrap_or(Duration::from_secs(60));
    let mut hb = tokio::time::interval(hb_interval);
    hb.tick().await; // interval 首次 tick 立即返回，先消费掉

    // 可选的演示控制指令（连接 2 秒后发送）
    let mut control = deviceid.zip(switch).map(|(id, sw)| {
        json!({
            "action": "update",
            "deviceid": id,
            "apikey": apikey,             // 自己的设备用登录返回的 apikey 即可
            "userAgent": "app",
            "sequence": sequence(),
            "params": { "switch": sw },   // "on" / "off"
        })
    });
    let control_delay = tokio::time::sleep(Duration::from_secs(2));
    tokio::pin!(control_delay);

    println!("监听设备状态中... Ctrl+C 退出");
    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<Value>(&text) {
                            Ok(v) => println!("<< {}", serde_json::to_string_pretty(&v)?),
                            Err(_) => println!("<< 非 JSON 消息: {text}"),
                        }
                    }
                    Some(Ok(Message::Ping(p))) => {
                        write.send(Message::Pong(p)).await?;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(_)) => {}
                    Some(Err(e)) => bail!("websocket 错误: {e}"),
                    None => bail!("连接已关闭"),
                }
            }
            _ = &mut control_delay, if control.is_some() => {
                let msg = control.take().expect("checked");
                write.send(Message::Text(msg.to_string())).await?;
                println!(">> 控制指令已发送: {msg}");
            }
            _ = hb.tick() => {
                write.send(Message::Ping(Vec::new())).await?;
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\n退出");
                break;
            }
        }
    }
    Ok(())
}
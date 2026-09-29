//! eWelink-rs：eWeLink（酷宅云）开放平台 v2 API 的 Rust 命令行客户端
//!
//! 用法（凭证通过环境变量提供）：
//!   ewelink-rs login               登录并输出 at/rt
//!   ewelink-rs refresh             用 rt 刷新 at
//!   ewelink-rs list                列出账号下所有设备
//!   ewelink-rs on <deviceid>       打开插座
//!   ewelink-rs off <deviceid>      关闭插座
//!   ewelink-rs status <deviceid>   查询插座状态
//!   ewelink-rs ws [deviceid] [on|off]  建立长连接；带 deviceid+开关时演示 WS 控制

mod api;
#[cfg(unix)]
mod daemon;
mod lan;
mod models;
mod sign;
mod ws;

use anyhow::{bail, Context, Result};
use std::time::Duration;

fn print_device(d: &models::Device) {
    let online = match d.online {
        Some(true) => "在线",
        Some(false) => "离线",
        None => "未知",
    };
    let uiid = d
        .extra
        .as_ref()
        .and_then(|e| e.uiid)
        .map(|u| u.to_string())
        .unwrap_or_else(|| "-".into());
    println!("  {}  deviceid={}  uiid={}  {}", d.name, d.deviceid, uiid, online);
    println!("      params: {}", d.params);
}

fn usage() {
    println!("用法: ewelink-rs <命令> [参数]");
    println!("  云端（走 eWeLink v2 API，注意免费账号调用配额）:");
    println!("  login                登录并输出 at/rt");
    println!("  refresh              用 rt 刷新 at");
    println!("  list                 列出账号下所有设备");
    println!("  on <deviceid> [outlet]  打开插座（多通道排插可指定插位）");
    println!("  off <deviceid> [outlet] 关闭插座");
    println!("  status <deviceid>    查询插座状态");
    println!("  pulsecfg <id> <ms|off> [outlet]  配置设备原生 pulse（通电 ms 毫秒自动断电）");
    println!("  pulse <id> <ms> [outlet]  一体化触发：原生 pulse + 开指令，设备自断电");
    println!("  ws [deviceid] [on|off]  建立长连接监听；带 deviceid+开关时演示 WS 控制");
    println!();
    println!("  局域网（zeroconf 直连：不走云端，不受 API 配额限制）:");
    println!("  lan devices           发现局域网内 eWeLink 设备并显示实时状态");
    println!("  lan status <id>       读取设备实时状态（mDNS 快照）");
    println!("  lan on|off <id> [outlet]  控制插位；不指定 outlet 时 4 通道同时操作");
    println!("  lan pulse <id> <ms> [outlet]  开启后保持 <ms> 毫秒自动关闭（守护进程内定时）");
    #[cfg(unix)]
    println!("  daemon                启动常驻局域网守护进程（自动拉起，一般无需手动）");
    println!();
    println!("环境变量:");
    println!("  EWELINK_APPID         你的 APPID（必填）");
    println!("  EWELINK_APPSECRET     App Secret（必填，HMAC-SHA256 签名用）");
    println!("  EWELINK_ACCOUNT       eWeLink 账号：邮箱，或带区号手机号如 +8613800138000（必填）");
    println!("  EWELINK_PASSWORD      账号密码（必填）");
    println!("  EWELINK_COUNTRY_CODE  电话区号，默认 +86（登录参数）");
    println!("  EWELINK_REDIRECT_URL   OAuth2.0 回调地址，须与 dev.ewelink.cc 应用管理登记的跳转地址一致（默认 https://web.ewelink.cc）");
    println!("  EWELINK_REGION        初始区域 cn/as/us/eu，默认 as；登录会自动重定向到账号所在区域");
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");

    if matches!(cmd, "help" | "-h" | "--help") {
        usage();
        return Ok(());
    }

    let mut client = api::Client::from_env()?;

    match cmd {
        "login" => {
            // 强制重新登录（清掉共享 token 缓存）
            let _ = std::fs::remove_file(api::token_path());
            client.login().await?;
            println!("at    = {}", client.at.as_deref().unwrap_or(""));
            println!("rt    = {}", client.rt.as_deref().unwrap_or(""));
            println!("region= {}", client.config.region);
        }
        "refresh" => {
            client.login().await?;
            client.refresh().await?;
            println!("新 at = {}", client.at.as_deref().unwrap_or(""));
            println!("新 rt = {}", client.rt.as_deref().unwrap_or(""));
        }
        "list" => {
            client.login().await?;
            let devices = client.list_devices().await?;
            println!("共 {} 台设备:", devices.len());
            for d in &devices {
                print_device(d);
            }
        }
        "on" | "off" => {
            let id = args.get(2).context("用法: ewelink-rs on <deviceid> [outlet]")?;
            client.login().await?;
            match args.get(3) {
                Some(outlet) => {
                    let outlet: u8 = outlet.parse().context("outlet 必须是 0-3 的数字")?;
                    client.set_outlet(id, outlet, cmd == "on").await?;
                    println!("已发送{}指令（插位 {}）", if cmd == "on" { "开" } else { "关" }, outlet);
                }
                None => {
                    client.set_switch(id, cmd == "on").await?;
                    println!("已发送{}指令", if cmd == "on" { "开" } else { "关" });
                }
            }
        }
        "status" => {
            let id = args.get(2).context("用法: ewelink-rs status <deviceid>")?;
            client.login().await?;
            let params = client.get_status(id).await?;
            println!("{}", serde_json::to_string_pretty(&params)?);
        }
        "pulsecfg" => {
            // 原生 pulse 配置：ms 为毫秒数；传 "off" 关闭
            let id = args.get(2).context("用法: ewelink-rs pulsecfg <deviceid> <ms|off> [outlet]")?;
            let ms = args.get(3).context("用法: ewelink-rs pulsecfg <deviceid> <ms|off> [outlet]")?;
            let outlet: u8 = match args.get(4) {
                Some(o) => o.parse().context("outlet 必须是 0-3 的数字")?,
                None => 0,
            };
            client.login().await?;
            let w = if ms == "off" {
                None
            } else {
                Some(ms.parse::<u64>().context("ms 需为毫秒数或 off")?)
            };
            let r = client.set_pulse(id, outlet, w).await?;
            println!(
                "已{}原生脉冲（插位 {}，{}）: {:?}",
                if w.is_some() { "启用" } else { "关闭" },
                outlet,
                match w {
                    Some(w) => format!("{w}ms 后自动断电"),
                    None => "关闭".to_string(),
                },
                r
            );
        }
        "pulse" => {
            // 一体化触发：启用原生 pulse + 发送开指令，设备自动在 ms 毫秒后断电（单次登录）
            // 省略 outlet = 全部插位（固件虚报多通道但物理仅一个插座时确保触发）
            let id = args.get(2).context("用法: ewelink-rs pulse <deviceid> <ms> [outlet]")?;
            let ms: u64 = args
                .get(3)
                .context("用法: ewelink-rs pulse <deviceid> <ms> [outlet]")?
                .parse()
                .context("ms 需为毫秒数")?;
            let outlet: Option<u8> = match args.get(4) {
                Some(o) => Some(o.parse().context("outlet 必须是 0-3 的数字")?),
                None => None,
            };
            client.login().await?;
            match outlet {
                Some(o) => {
                    client.set_pulse(id, o, Some(ms)).await?;
                    client.set_outlet(id, o, true).await?;
                    println!("已启用原生脉冲（插位 {o}，{ms}ms）并发送开指令——设备将自动断电");
                }
                None => {
                    let params = client.get_status(id).await?;
                    let n = params
                        .get("pulses")
                        .and_then(|p| p.as_array())
                        .map(|a| a.len())
                        .or_else(|| {
                            params
                                .get("switches")
                                .and_then(|s| s.as_array())
                                .map(|a| a.len())
                        })
                        .unwrap_or(4) as u8;
                    client.set_pulse_all(id, ms, n).await?;
                    client.set_switch(id, true).await?;
                    println!("已启用原生脉冲（全部插位，{ms}ms）并发送开指令——设备将自动断电");
                }
            }
        }
        "ws" => {
            client.login().await?;
            let id = args.get(2).map(String::as_str);
            let sw = args.get(3).map(String::as_str);
            ws::run(&client, id, sw).await?;
        }
        #[cfg(unix)]
        "daemon" => {
            daemon::run_daemon().await?;
        }
        #[cfg(not(unix))]
        "daemon" => bail!("daemon 模式仅支持 Unix（请使用 WSL）"),
        "lan" => {
            use std::time::Duration;
            let sub = args.get(2).map(String::as_str).unwrap_or("devices");
            match sub {
                "devices" | "list" => {
                    let devs = lan::discover(Duration::from_secs(8))?;
                    if devs.is_empty() {
                        bail!("局域网未发现 eWeLink 设备（请确认与设备在同一局域网，防火墙放行 UDP 5353）");
                    }
                    let ids: Vec<String> = devs.iter().map(|d| d.deviceid.clone()).collect();
                    let keys = lan::ensure_device_keys(&mut client, &ids).await?;
                    println!("共 {} 台设备（局域网直连）:", devs.len());
                    for d in &devs {
                        let key = match keys.get(&d.deviceid) {
                            Some(k) => k,
                            None => {
                                println!("  {}  deviceid={}  {}:{}", d.name, d.deviceid, d.addr, d.port);
                                println!("      (无 devicekey，跳过状态)");
                                continue;
                            }
                        };
                        println!(
                            "  {}  deviceid={}  {}:{}",
                            d.name, d.deviceid, d.addr, d.port
                        );
                        match lan::decrypt_snapshot(&d.props, key) {
                            Ok(v) => println!(
                                "      switches: {}",
                                v.get("switches").cloned().unwrap_or(serde_json::Value::Null)
                            ),
                            Err(e) => println!("      (无状态快照: {e})"),
                        }
                    }
                }
                "status" => {
                    let id = args.get(3).context("用法: ewelink-rs lan status <deviceid>")?;
                    // 常驻守护优先：命中热缓存时毫秒级返回
                    if let Some(resp) = try_daemon(&format!("status {id}"), Duration::from_secs(15))
                    {
                        if resp.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
                            println!("{}  {}", resp["name"], resp["addr"]);
                            println!("{}", serde_json::to_string_pretty(&resp["snapshot"])?);
                            return Ok(());
                        }
                    }
                    // 回退：本地直连全流程
                    let dev = lan::resolve_one_retry(id, Duration::from_secs(5))?;
                    let keys = lan::ensure_device_keys(&mut client, &[id.to_string()]).await?;
                    let snap = lan::decrypt_snapshot(&dev.props, &keys[id])?;
                    println!("{}  {}:{}", dev.name, dev.addr, dev.port);
                    println!("{}", serde_json::to_string_pretty(&snap)?);
                }
                "on" | "off" => {
                    let id = args
                        .get(3)
                        .context("usage: ewelink-rs lan on|off <deviceid> [outlet]")?;
                    let keys = lan::ensure_device_keys(&mut client, &[id.to_string()]).await?;
                    let on = sub == "on";
                    let outlet_arg = args
                        .get(4)
                        .map(|o| {
                            let outlet: u8 = o.parse().context("outlet 必须是从 0 开始的整数")?;
                            Ok::<_, anyhow::Error>(outlet)
                        })
                        .transpose()?;
                    // 常驻守护优先：命中热缓存后 on/off 仅剩 HTTP 往返 + 广播确认
                    let daemon_cmd = {
                        let mut c = format!("{} {id}", if on { "on" } else { "off" });
                        if let Some(o) = outlet_arg {
                            c.push_str(&format!(" {o}"));
                        }
                        c
                    };
                    if let Some(resp) = try_daemon(&daemon_cmd, Duration::from_secs(20)) {
                        if resp["ok"].as_bool().unwrap_or(false) {
                            println!("{}", resp["sent"].as_str().unwrap_or(""));
                            println!("{}", resp["confirm_msg"].as_str().unwrap_or(""));
                            return Ok(());
                        }
                        // 守护在跑但执行失败（如 deviceid 缺失）→ 交给本地逻辑再次尝试
                    }
                    // 回退：本地直连全流程
                    let (dev, ts) = lan::LanCache::get(id, Duration::from_secs(5))?;
                    match outlet_arg {
                        // 不指定插位：全部插位同时操作（硬件实为单槽位）
                        None => {
                            lan::set_all_switches(&dev, &keys[id], on).await?;
                            println!(
                                "LAN 全部插位{} 已发送到 {} ({})",
                                if on { "开" } else { "关" },
                                dev.deviceid,
                                dev.host_port()
                            );
                            // 等设备状态变化的主动广播（<1s 通常可达），比对确认
                            let (ok, snap) = match lan::LanCache::wait_refresh(
                                id,
                                ts,
                                Duration::from_secs(3),
                            ) {
                                Some(dev2) => lan::check_snapshot(&dev2, &keys[id], on, None),
                                None => (false, None),
                            };
                            print_confirm(ok, snap, on, None);
                        }
                        Some(outlet) => {
                            lan::set_switches(&dev, &keys[id], outlet, on).await?;
                            println!(
                                "LAN {}(outlet={}) 已发送到 {} ({})",
                                if on { "开" } else { "关" },
                                outlet,
                                dev.deviceid,
                                dev.host_port()
                            );
                            let (ok, snap) = match lan::LanCache::wait_refresh(
                                id,
                                ts,
                                Duration::from_secs(3),
                            ) {
                                Some(dev2) => {
                                    lan::check_snapshot(&dev2, &keys[id], on, Some(outlet))
                                }
                                None => (false, None),
                            };
                            print_confirm(ok, snap, on, Some(outlet));
                        }
                    }
                }
                "pulse" => {
                    let id = args
                        .get(3)
                        .context("用法: ewelink-rs lan pulse <deviceid> <ms> [outlet]")?;
                    let ms: u64 = args
                        .get(4)
                        .context("用法: ewelink-rs lan pulse <deviceid> <ms> [outlet]")?
                        .parse()
                        .context("时长必须是毫秒整数")?;
                    let outlet_arg = args
                        .get(5)
                        .map(|o| {
                            let outlet: u8 =
                                o.parse().context("outlet 必须是从 0 开始的整数")?;
                            Ok::<_, anyhow::Error>(outlet)
                        })
                        .transpose()?;
                    // 守护优先：固件原生 pulse（配置 + 开指令，设备自动断电，立即返回）
                    let daemon_cmd = {
                        let mut c = format!("pulse {id} {ms}");
                        if let Some(o) = outlet_arg {
                            c.push_str(&format!(" {o}"));
                        }
                        c
                    };
                    if let Some(resp) = try_daemon(&daemon_cmd, Duration::from_secs(30)) {
                        if resp["ok"].as_bool().unwrap_or(false) {
                            println!("固件原生脉冲({ms}ms) 已触发 {}: {}", id, resp["msg"].as_str().unwrap_or(""));
                            return Ok(());
                        }
                        let err = resp["error"].as_str().unwrap_or("未知错误");
                        println!("守护执行失败: {err}（回退本地执行）");
                    }
                    // 回退：本地直接固件 pulse（LAN 下发 pulses + 开，设备自动断电）
                    let keys = lan::ensure_device_keys(&mut client, &[id.to_string()]).await?;
                    let (dev, _) = lan::LanCache::get(id, Duration::from_secs(5))?;
                    match outlet_arg {
                        None => {
                            lan::set_pulse_all(&dev, &keys[id], ms).await?;
                            lan::set_all_switches(&dev, &keys[id], true).await?;
                            println!("LAN 固件原生脉冲({ms}ms, 全部插位) 已触发 {}", dev.deviceid);
                        }
                        Some(o) => {
                            lan::set_pulse(&dev, &keys[id], o, Some(ms)).await?;
                            lan::set_switches(&dev, &keys[id], o, true).await?;
                            println!("LAN 固件原生脉冲({ms}ms, outlet={o}) 已触发 {}", dev.deviceid);
                        }
                    }
                }
                _ => usage(),
            }
        }
        _ => usage(),
    }
    Ok(())
}

/// 尝试经常驻守护执行指令：守护不在则自动拉起，返回解析后的 JSON 响应。
#[cfg(unix)]
fn try_daemon(cmd: &str, resp_timeout: Duration) -> Option<serde_json::Value> {
    if daemon::ensure_daemon() {
        if let Some(s) = daemon::dispatch(cmd, resp_timeout) {
            return serde_json::from_str(&s).ok();
        }
    }
    None
}

/// 输出控制后的回读确认结果：达成 / 不一致 / 无法回读（设备已确认执行但快照暂不可得）。
fn print_confirm(ok: bool, snap: Option<serde_json::Value>, on: bool, outlet: Option<u8>) {
    let action = if on { "开" } else { "关" };
    let scope = outlet
        .map(|o| format!("outlet {o}"))
        .unwrap_or_else(|| "全部插位".to_string());
    let target = match snap.as_ref() {
        Some(v) => v
            .get("switches")
            .and_then(|s| s.as_array())
            .map(|a| serde_json::Value::Array(a.clone()))
            .unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::Null,
    };
    match (ok, snap) {
        (true, _) => println!("✓ 已回读确认：{scope}已{action}"),
        (false, Some(_)) => println!("⚠ 设备已执行 (error:0)，但回读状态与目标不一致: {target}"),
        (false, None) => println!(
            "（未能回读确认：mDNS 快照暂不可得；设备已返回 error:0 确认执行）"
        ),
    }
}
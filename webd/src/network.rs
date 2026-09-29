//! 网络信息：网卡 IP（SIOCGIFADDR）+ 默认路由 + WiFi 模式（wificfgd 标记文件约定）。

use std::path::Path;

use serde_json::{json, Value};

use crate::config::Config;

/// 读取某网卡的 IPv4 地址（ioctl SIOCGIFADDR），失败返回 None
fn iface_ipv4(name: &str) -> Option<String> {
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if fd < 0 {
            return None;
        }
        let mut ifr: libc::ifreq = std::mem::zeroed();
        let bytes = name.as_bytes();
        if bytes.len() >= ifr.ifr_name.len() {
            libc::close(fd);
            return None;
        }
        for (i, b) in bytes.iter().enumerate() {
            ifr.ifr_name[i] = *b as libc::c_char;
        }
        // 请求码类型随目标不同（glibc: u64 / musl: i32），统一 try_into 转换
        let rc = libc::ioctl(fd, libc::SIOCGIFADDR.try_into().unwrap(), &mut ifr);
        libc::close(fd);
        if rc != 0 {
            return None;
        }
        let addr = ifr.ifr_ifru.ifru_addr;
        let sin = &*(&addr as *const libc::sockaddr as *const libc::sockaddr_in);
        let ip = u32::from_be(sin.sin_addr.s_addr);
        Some(format!("{}.{}.{}.{}", (ip >> 24) & 0xff, (ip >> 16) & 0xff, (ip >> 8) & 0xff, ip & 0xff))
    }
}

/// 枚举 /sys/class/net/*，返回 {name, mac, ip4}
pub fn list_interfaces(sys_class_net: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(sys_class_net) else {
        return out;
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n != "lo")
        .collect();
    names.sort();
    for name in names {
        let mac = std::fs::read_to_string(format!("{sys_class_net}/{name}/address"))
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let ip4 = iface_ipv4(&name);
        if ip4.is_none() && mac.is_empty() {
            continue; // 无地址的虚拟网卡跳过
        }
        out.push(json!({
            "name": name,
            "mac": mac,
            "ip4": ip4,
        }));
    }
    out
}

/// 默认网关接口名（/proc/net/route 文本中 dest=00000000 的行）
pub fn parse_default_gw(text: &str) -> Option<String> {
    for line in text.lines().skip(1) {
        let mut it = line.split_whitespace();
        let iface = it.next()?;
        if it.next() == Some("00000000") {
            return Some(iface.to_string());
        }
    }
    None
}

/// WiFi 模式：按 wificfgd 约定读 boot 分区标记文件
pub fn wifi_mode(boot_dir: &str) -> Value {
    let ap = Path::new(boot_dir).join("wifi.ap").exists();
    let sta = Path::new(boot_dir).join("wifi.sta").exists();
    let ssid = std::fs::read_to_string(Path::new(boot_dir).join("wifi.ssid"))
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let mode = if ap {
        "config_ap"
    } else if sta {
        "sta"
    } else {
        "unknown"
    };
    json!({
        "mode": mode,
        "configured_ssid": ssid,
    })
}

/// 汇总网络信息
pub async fn collect(cfg: &Config) -> Value {
    let ifaces = list_interfaces("/sys/class/net");
    let route = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
    let gw = parse_default_gw(&route);
    let wifi = wifi_mode(&cfg.boot_dir.to_string_lossy());
    json!({
        "interfaces": ifaces,
        "default_gw_iface": gw,
        "wifi": wifi,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_parse_gw() {
        let text = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                    eth0\t00000000\t0A0A0A01\t0003\t0\t0\t0\t00000000\t0\t0\t0\n\
                    eth0\t000A0A0A\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";
        assert_eq!(parse_default_gw(text), Some("eth0".to_string()));
    }

    #[test]
    fn route_parse_no_gw() {
        assert_eq!(parse_default_gw("Iface\tDestination\neth0\t000A0A0A\n"), None);
    }
}

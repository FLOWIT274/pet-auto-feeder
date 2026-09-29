//! 系统概览：/proc 解析 + 磁盘 + CPU 差分采样 + 温度。
//! 解析函数全部接受 &str，便于宿主单测。

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

/// CPU 差分采样状态：上一次 /proc/stat 的 (user, nice, system, idle, total)
#[derive(Default)]
pub struct CpuState {
    prev: Option<(u64, u64, u64, u64, u64)>,
}

pub fn parse_meminfo(text: &str) -> HashMap<String, u64> {
    let mut m = HashMap::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if let Some(key) = it.next() {
            if let Some(v) = it.next().and_then(|s| s.parse::<u64>().ok()) {
                m.insert(key.trim_end_matches(':').to_string(), v);
            }
        }
    }
    m
}

pub fn parse_loadavg(text: &str) -> Vec<f64> {
    text.split_whitespace()
        .take(3)
        .filter_map(|s| s.parse::<f64>().ok())
        .collect()
}

pub fn parse_uptime(text: &str) -> u64 {
    text.split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|f| f as u64)
        .unwrap_or(0)
}

/// 解析 /proc/stat 第一行，返回 (user, nice, system, idle, total)
pub fn parse_proc_stat_first(text: &str) -> Option<(u64, u64, u64, u64, u64)> {
    let line = text.lines().next()?;
    let mut it = line.split_whitespace();
    if it.next()? != "cpu" {
        return None;
    }
    let mut vals = [0u64; 4];
    for v in vals.iter_mut() {
        *v = it.next()?.parse().ok()?;
    }
    let user = vals[0];
    let nice = vals[1];
    let system = vals[2];
    let idle = vals[3];
    let total = user + nice + system + idle + it.filter_map(|s| s.parse::<u64>().ok()).sum::<u64>();
    Some((user, nice, system, idle, total))
}

/// 读 /proc/stat 并相对上次调用计算 CPU 占用百分比（0.0 ~ 100.0）
pub fn cpu_percent(state: &Mutex<CpuState>, stat_text: &str) -> Option<f64> {
    let cur = parse_proc_stat_first(stat_text)?;
    let mut st = state.lock().unwrap();
    match st.prev {
        Some(prev) => {
            let d_user = cur.0.saturating_sub(prev.0);
            let d_sys = cur.2.saturating_sub(prev.2);
            let d_idle = cur.3.saturating_sub(prev.3);
            let _ = d_idle;
            let d_total = cur.4.saturating_sub(prev.4);
            st.prev = Some(cur);
            if d_total == 0 {
                Some(0.0)
            } else {
                Some(100.0 * (d_user + d_sys) as f64 / d_total as f64)
            }
        }
        None => {
            st.prev = Some(cur);
            None
        }
    }
}

/// 磁盘使用（statvfs）。返回 {total_bytes, free_bytes, used_pct}
pub fn disk_usage(path: &str) -> Option<Value> {
    use std::mem::MaybeUninit;
    unsafe {
        let mut st: libc::statvfs = MaybeUninit::zeroed().assume_init();
        let cpath = std::ffi::CString::new(path).ok()?;
        if libc::statvfs(cpath.as_ptr(), &mut st) != 0 {
            return None;
        }
        let total = st.f_blocks as u64 * st.f_frsize as u64;
        let free = st.f_bavail as u64 * st.f_frsize as u64;
        let used = total.saturating_sub(free);
        Some(json!({
            "path": path,
            "total_bytes": total,
            "free_bytes": free,
            "used_pct": if total == 0 { 0.0 } else { 100.0 * used as f64 / total as f64 },
        }))
    }
}

/// 读取 CPU 温度（/sys/class/thermal/thermal_zone0/temp，毫摄氏度），失败返回 None
pub fn cpu_temp() -> Option<f64> {
    let raw = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp").ok()?;
    let millic = raw.trim().parse::<f64>().ok()?;
    Some(millic / 1000.0)
}

/// 汇总系统信息（path_base 默认 /proc，测试可注入）
pub async fn collect(path_base: &str, cpu: &Mutex<CpuState>) -> Value {
    let meminfo = tokio::fs::read_to_string(format!("{path_base}/meminfo"))
        .await
        .unwrap_or_default();
    let mem = parse_meminfo(&meminfo);
    let loadavg = tokio::fs::read_to_string(format!("{path_base}/loadavg"))
        .await
        .unwrap_or_default();
    let uptime = tokio::fs::read_to_string(format!("{path_base}/uptime"))
        .await
        .unwrap_or_default();
    let stat = tokio::fs::read_to_string(format!("{path_base}/stat"))
        .await
        .unwrap_or_default();
    let hostname = tokio::fs::read_to_string("/proc/sys/kernel/hostname")
        .await
        .unwrap_or_default();

    let g = |k: &str| mem.get(k).copied().unwrap_or(0);
    let mut disks = vec![disk_usage("/")];
    if std::path::Path::new("/mnt/data").exists() {
        disks.push(disk_usage("/mnt/data"));
    }
    let disks: Vec<Value> = disks.into_iter().flatten().collect();

    json!({
        "hostname": hostname.trim().to_string(),
        "uptime_secs": parse_uptime(&uptime),
        "loadavg": parse_loadavg(&loadavg),
        "cpu_percent": cpu_percent(cpu, &stat),
        "mem": {
            "total_kb": g("MemTotal"),
            "avail_kb": g("MemAvailable"),
        },
        "swap": {
            "total_kb": g("SwapTotal"),
            "free_kb": g("SwapFree"),
        },
        "disks": disks,
        "temp_c": cpu_temp(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_parse() {
        let m = parse_meminfo("MemTotal:       167796 kB\nMemAvailable:   123456 kB\nSwapTotal: 2097152 kB\n");
        assert_eq!(m.get("MemTotal"), Some(&167796));
        assert_eq!(m.get("SwapTotal"), Some(&2097152));
        assert_eq!(m.get("MemAvailable"), Some(&123456));
    }

    #[test]
    fn loadavg_parse() {
        assert_eq!(parse_loadavg("0.52 0.31 0.19 1/123 456"), vec![0.52, 0.31, 0.19]);
    }

    #[test]
    fn uptime_parse() {
        assert_eq!(parse_uptime("12345.67 98765.43"), 12345);
    }

    #[test]
    fn proc_stat_cpu() {
        let t1 = "cpu  1000 0 500 8000 0 0 0 0 0 0\ncpu0 100 0 50 800\n";
        let t2 = "cpu  1100 0 550 8600 0 0 0 0 0 0\ncpu0 100 0 50 800\n";
        let st = Mutex::new(CpuState::default());
        // 第一次无基线 → None
        assert!(cpu_percent(&st, t1).is_none());
        // 第二次：user+100 sys+50 idle+600 → 150/750 = 20%
        let p = cpu_percent(&st, t2).unwrap();
        assert!((p - 20.0).abs() < 0.01, "p={p}");
    }
}

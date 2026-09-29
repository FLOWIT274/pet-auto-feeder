# 回退判定：扫描窗口内是否存在可用网络
# 被 S30wifi 引用；可注入变量（测试用）：
#   WIFIFB_WINDOW      扫描总窗口秒数（默认 10）
#   WIFIFB_INTERVAL    扫描间隔秒数（默认 3）
#   WIFIFB_SCAN_CMD    触发扫描命令（默认 wpa_cli -i wlan0 scan）
#   WIFIFB_SCAN_RESULTS_CMD  读取扫描结果（默认 wpa_cli -i wlan0 scan_results）
# 返回 0 = 窗口内有网络（继续 STA）；1 = 无网络（应回退 AP）
should_fallback() {
    local window=${WIFIFB_WINDOW:-10}
    local interval=${WIFIFB_INTERVAL:-3}
    local scan_cmd="${WIFIFB_SCAN_CMD:-wpa_cli -i wlan0 scan}"
    local results_cmd="${WIFIFB_SCAN_RESULTS_CMD:-wpa_cli -i wlan0 scan_results}"
    local deadline=$(( $(date +%s) + window ))
    local out

    while [ "$(date +%s)" -lt "$deadline" ]; do
        # 触发扫描（异步完成，稍候再读结果）
        eval "$scan_cmd" >/dev/null 2>&1
        sleep 1
        # 读结果：跳过表头行（以 bssid/ 开头），其余均为真实 AP
        out=$(eval "$results_cmd" 2>/dev/null | grep -v '^bssid /' | grep -v '^$')
        if [ -n "$out" ]; then
            return 0
        fi
        sleep "$interval"
    done
    return 1
}
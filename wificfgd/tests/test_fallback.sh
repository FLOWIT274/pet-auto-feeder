#!/bin/sh
# 切片 S5 测试：回退判定函数 should_fallback（宿主模拟扫描输出）
# 用法：source wifi_fallback.sh 后调用；窗口/间隔可注入小值加速测试
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/../src/wifi_fallback.sh"

fail=0; checks=0
check() { checks=$((checks+1)); if [ "$1" != "$2" ]; then fail=1; echo "FAIL: $3 (期望 $2 实际 $1)"; fi; }

# --- 场景 1：扫描始终无网络 → 回退（返回 1）---
WIFIFB_WINDOW=2 WIFIFB_INTERVAL=1 \
WIFIFB_SCAN_CMD="true" \
WIFIFB_SCAN_RESULTS_CMD="echo 'bssid / frequency / signal / flags / ssid'" \
should_fallback
check $? 1 "始终无网络应回退"

# --- 场景 2：第二次扫描出现网络 → 不回退（返回 0）---
WIFIFB_WINDOW=5 WIFIFB_INTERVAL=1 \
WIFIFB_SCAN_CMD="true" \
WIFIFB_SCAN_RESULTS_CMD='if [ ! -f "${TMPDIR:-/tmp}/wificfgd-s5_hit" ]; then echo "bssid / frequency / signal / flags / ssid"; echo; touch "${TMPDIR:-/tmp}/wificfgd-s5_hit"; else echo "bssid / frequency / signal / flags / ssid"; echo "aa:bb:cc:dd:ee:ff 2412 -40 [WPA2] MyWiFi"; fi' \
should_fallback
check $? 0 "扫描到网络不应回退"
rm -f ${TMPDIR:-/tmp}/wificfgd-s5_hit

# --- 场景 3：单行表头不算网络（无实际条目仍回退）---
WIFIFB_WINDOW=2 WIFIFB_INTERVAL=1 \
WIFIFB_SCAN_CMD="true" \
WIFIFB_SCAN_RESULTS_CMD="echo 'bssid / frequency / signal / flags / ssid'" \
should_fallback
check $? 1 "纯表头不算网络"

# --- 场景 4：wpa_cli 不存在/失败视为无网络 → 回退 ---
WIFIFB_WINDOW=2 WIFIFB_INTERVAL=1 \
WIFIFB_SCAN_CMD="false" \
WIFIFB_SCAN_RESULTS_CMD="false" \
should_fallback
check $? 1 "扫描命令失败应回退"

echo "S5: $checks checks, $([ $fail -eq 0 ] && echo ALL GREEN || echo FAILURES)"
exit $fail
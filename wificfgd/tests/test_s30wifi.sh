#!/bin/sh
# S30wifi 集成测试：should_fallback 返回值 → start() 分支选择
# 覆盖实机 bug：should_fallback 返回 0（有网络）时旧代码误走回退 AP 分支
# 方法：S30wifi 复制到临时目录，source 行剥离 + /boot 注入 FAKE_BOOT_DIR + 函数 fake
# S30wifi 来源：优先用仓库内 device/overlay 副本，可用 S30_SRC 环境变量覆盖
HERE=$(cd "$(dirname "$0")" && pwd)
S30_SRC=${S30_SRC:-"$HERE/../../device/overlay/etc/init.d/S30wifi"}

fail=0; checks=0
check() { checks=$((checks+1)); if [ "$1" != "$2" ]; then fail=1; echo "FAIL: $3 (期望 [$2] 实际 [$1])"; fi; }

TMPD=$(mktemp -d "${TMPDIR:-/tmp}/wificfgd-s30.XXXXXX")
S30_FAKE=$TMPD/S30wifi.fake

run_start() {
    # $1=FAKE_RC(should_fallback 返回值), $2=是否创建 wifi.ap 标记, $3=nokey 则不创建 wifi.ssid/pass
    # 注意：命令替换 $(run_start ...) 在子 shell 执行，全局变量不回流 → 每场景必须 mktemp 独立目录
    local FAKE_RC=$1 AP_MARK=$2 KEYS=$3
    local FB=$(mktemp -d "$TMPD/boot.XXXXXX")
    mkdir -p "$FB"
    if [ "$AP_MARK" = "yes" ]; then touch "$FB/wifi.ap"; fi
    if [ "$KEYS" != "nokey" ]; then echo testssid > "$FB/wifi.ssid"; echo testpass > "$FB/wifi.pass"; fi
    sed -e '/^\. \/etc\/profile$/d' -e '/^\. \/etc\/wifi_fallback.sh$/d' -e "s|/boot|$FB|g" "$S30_SRC" > "$S30_FAKE"
    # 先 source S30wifi（仅加载定义），再覆盖函数 → 后定义者胜
    sh -c ". $S30_FAKE
FAKE_RC=$FAKE_RC
start_sta() { echo \"FAKE: start_sta\"; }
stop_wifi_sta() { echo \"FAKE: stop_wifi_sta\"; }
start_ap() { echo \"FAKE: start_ap\"; }
should_fallback() { echo \"FAKE: should_fallback rc=$FAKE_RC\"; return $FAKE_RC; }
start"
}

# --- 场景 1：有网络（should_fallback=0）→ 保持 STA，绝不回退 ---
out1=$(run_start 0 no)
check "$(echo "$out1" | grep -c 'staying in STA mode')" "1" "有网络应保持 STA"
check "$(echo "$out1" | grep -c 'FAKE: start_ap')" "0" "有网络不得启动 AP"
check "$(echo "$out1" | grep -c 'FAKE: start_sta')" "1" "有网络应先启动 STA"

# --- 场景 2：无网络（should_fallback=1）→ 回退 AP ---
out2=$(run_start 1 no)
check "$(echo "$out2" | grep -c 'wifi fallback')" "1" "无网络应回退"
check "$(echo "$out2" | grep -c 'FAKE: start_ap')" "1" "无网络应启动 AP"
check "$(echo "$out2" | grep -c 'FAKE: stop_wifi_sta')" "1" "无网络应停 STA"

# --- 场景 3：wifi.ap 标记 → 直接 AP，不调 should_fallback ---
out3=$(run_start 99 yes)
check "$(echo "$out3" | grep -c 'FAKE: start_ap')" "1" "wifi.ap 标记直接 AP"
check "$(echo "$out3" | grep -c 'FAKE: should_fallback')" "0" "wifi.ap 标记不扫描"

# --- 场景 4：无凭证（无 wifi.ssid/pass）→ 直接进 AP 配置窗口期 ---
out4=$(run_start 99 no nokey)
check "$(echo "$out4" | grep -c 'no credentials, switching to AP')" "1" "无凭证直接切 AP"
check "$(echo "$out4" | grep -c 'FAKE: start_ap')" "1" "无凭证启动 AP"
check "$(echo "$out4" | grep -c 'FAKE: start_sta')" "0" "无凭证不试 STA"
check "$(echo "$out4" | grep -c 'FAKE: should_fallback')" "0" "无凭证不扫描"

rm -rf "$TMPD"
echo "S30-integ: $checks checks, $([ $fail -eq 0 ] && echo ALL GREEN || echo FAILURES)"
exit $fail

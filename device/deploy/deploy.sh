#!/bin/bash
# deploy.sh — 把本仓库的 device/overlay 部署到运行中的板子（不重新打包固件时）
#
# 用法: ./deploy.sh [板子IP]
#   板子IP 缺省读 config.env 的 BOARD_IP，再缺省为 10.86.142.1 (USB RNDIS)
#
# 需要：
#   - sshpass（板子 root 密码默认 root）
#   - 已构建的二进制：vision/bin/vdec_stream_v4l2、webd/target/.../webd、ewelink/target/.../ewelink-rs
#     （缺失则跳过对应项并提示，脚本仍可用）
#   - 真实凭证：device/overlay/etc/ewelink.env、etc/webd.env、boot/wifi.ssid、boot/wifi.pass
#     （这些文件不在仓库内，见 *.env.example；本地放好后本脚本会一并推送）
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
[ -f "$REPO/config.env" ] && . "$REPO/config.env"

OVERLAY="$REPO/device/overlay"
DEV="${1:-${BOARD_IP:-10.86.142.1}}"
SSH="sshpass -p root ssh -o ConnectTimeout=8 -o StrictHostKeyChecking=no root@$DEV"
SCP="sshpass -p root scp -o ConnectTimeout=10 -o StrictHostKeyChecking=no"

echo "== overlay: $OVERLAY =="
$SSH "echo 板子在线: $DEV"

# 1) 停服务（避免 Text file busy）
$SSH "/etc/init.d/S90webd stop >/dev/null 2>&1; /etc/init.d/S99keywipe stop >/dev/null 2>&1; killall -9 ewelink-rs 2>/dev/null; true"

echo "== 部署前状态 =="
$SSH "free | head -2; cat /proc/loadavg; dmesg | tail -2" 2>/dev/null || true

# 2) rootfs 脚本与配置（只推仓库里存在的：凭证文件属于本地私有，缺失就跳过）
FILES="etc/init.d/S20buzzer etc/init.d/S30wifi etc/init.d/S49ntp etc/init.d/S90ewelink etc/init.d/S90webd \
       etc/init.d/S96vision etc/init.d/S97logpersist etc/init.d/S97wifidisc etc/init.d/S98wifiguard etc/init.d/S99keywipe \
       usr/bin/key-wipe.py usr/bin/wifidisc.py usr/bin/buzzer_beep.sh usr/bin/crashlog.sh \
       etc/ntp.conf etc/webd.env etc/ewelink.env \
       boot/wifi.ssid boot/wifi.pass"

# 取回板上 md5；内容相同的跳过（避免反复写大文件进 SD）
declare -A RMOTE=()
while read -r sum path; do
    [ -n "$path" ] && RMOTE["${path#/}"]="$sum"
done < <($SSH "for p in $FILES; do [ -f \"/\$p\" ] && md5sum \"/\$p\"; done" 2>/dev/null || true)

for f in $FILES; do
    if [ ! -f "$OVERLAY/$f" ]; then
        echo "  - $f (本地无此文件: 需从 *.example 复制并填真实值)"
        continue
    fi
    lsum=$(md5sum "$OVERLAY/$f" | awk '{print $1}')
    if [ "${RMOTE[$f]:-}" = "$lsum" ]; then
        echo "  = $f (未变，跳过)"
    else
        $SCP "$OVERLAY/$f" "root@$DEV:/$f"
        echo "  ✓ $f"
    fi
done
# 脚本类必须可执行
$SSH "chmod 755 /usr/bin/key-wipe.py /usr/bin/wifidisc.py /usr/bin/buzzer_beep.sh /usr/bin/crashlog.sh \
      /etc/init.d/S20buzzer /etc/init.d/S30wifi /etc/init.d/S49ntp /etc/init.d/S90ewelink /etc/init.d/S90webd \
      /etc/init.d/S96vision /etc/init.d/S97logpersist /etc/init.d/S97wifidisc /etc/init.d/S98wifiguard /etc/init.d/S99keywipe 2>/dev/null; sync"

# 3) 二进制。注意必须落在 /mnt/system/usr/bin/：S90webd / S90ewelink 按该路径调用
#    （/mnt/system 是 rootfs 上的普通目录，不是独立分区；放 usr/bin/ 服务起不来）
push_bin() {
    local src=$1 dst=$2 name=$3
    if [ ! -f "$src" ]; then echo "  - $name (未构建，跳过)"; return 0; fi
    local lsum rsum
    lsum=$(md5sum "$src" | awk '{print $1}')
    rsum=$($SSH "[ -f '$dst' ] && md5sum '$dst'" 2>/dev/null | awk '{print $1}' || true)
    if [ "$lsum" = "$rsum" ]; then
        echo "  = $name (未变，跳过)"
    else
        $SSH "mkdir -p $(dirname "$dst")"
        $SCP "$src" "root@$DEV:$dst"
        echo "  ✓ $name"
    fi
}

push_bin "$REPO/webd/target/riscv64gc-unknown-linux-musl/release/webd"          /mnt/system/usr/bin/webd        webd
push_bin "$REPO/ewelink/target/riscv64gc-unknown-linux-musl/release/ewelink-rs" /mnt/system/usr/bin/ewelink-rs   ewelink-rs

# 4) 视觉二进制 + 模型（data 分区；先停实例防 Text file busy）
$SSH "mkdir -p /mnt/data/tpu/models; /etc/init.d/S96vision stop >/dev/null 2>&1; killall -9 vdec_stream_v4l2 vdec_stream 2>/dev/null; true"
push_bin "$REPO/vision/bin/vdec_stream_v4l2" /mnt/data/tpu/vdec_stream_v4l2 vdec_stream_v4l2
if [ -f "$REPO/vision/yolov8n_det_pet_person_384_640_INT8_cv181x.cvimodel" ]; then
    $SCP "$REPO/vision/yolov8n_det_pet_person_384_640_INT8_cv181x.cvimodel" "root@$DEV:/mnt/data/tpu/models/" \
        && echo "  ✓ 模型"
else
    echo "  - 模型 (仓库内无，需从 TPU SDK 取或另行分发)"
fi

# 5) 启动服务并自检
$SSH "
  rm -f /root/.ewelink-rs-daemon.sock
  /etc/init.d/S20buzzer start
  /etc/init.d/S90ewelink start
  /etc/init.d/S90webd start
  /etc/init.d/S98wifiguard start
  /etc/init.d/S99keywipe start
  /etc/init.d/S96vision start
  /etc/init.d/S97logpersist start
  sleep 2
  echo '== 服务状态 =='
  ps -ef | grep -E 'ewelink-rs daemon|/usr/bin/webd|key-wipe.py|vdec_stream_v4l2' | grep -v grep
  wget -q -O /dev/null http://127.0.0.1:8080/api/health && echo '管理台 OK' || echo '管理台 未就绪'
  [ -x /usr/bin/buzzer_beep.sh ] && echo '蜂鸣器脚本 OK' || echo '蜂鸣器脚本缺失/不可执行'
  /etc/init.d/S97logpersist status
"
echo "== 部署完成 =="

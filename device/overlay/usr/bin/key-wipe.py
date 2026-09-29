#!/usr/bin/env python3
# key-wipe 守护：长按 User Key（/dev/input/event0）5 秒 → 清空 wlan 配网信息
# 清空 /boot/wifi.ssid /boot/wifi.pass /boot/wifi.sta /etc/wpa_supplicant.conf
# 然后重启 WiFi：无凭证 → 固件自动进入 AP 配置模式（licheervnano-XXXX 热点 + 80 端口配置页）
# 触发后 60s 冷却，防止重复触发
# 状态实时写入 /tmp/keywipe_state.json 供管理台可视化
import json
import os
import select
import struct
import subprocess
import sys
import time

EV = os.environ.get("KEY_WIPE_DEV", "/dev/input/event0")
TRIGGER_SEC = 5.0
COOLDOWN_SEC = 60.0
LOG = "/var/log/keywipe.log"
STATE = "/tmp/keywipe_state.json"
WIPE_FILES = [
    "/boot/wifi.ssid",
    "/boot/wifi.pass",
    "/boot/wifi.sta",
    "/etc/wpa_supplicant.conf",
]

state = {
    "pressed": False,
    "press_start_ms": 0,
    "pressed_for_ms": 0,
    "last_wipe_ms": 0,
    "history": [],  # [{ts, type: press|release|wipe, dur_ms}]
}

def write_state():
    try:
        with open(STATE, "w") as f:
            json.dump(state, f)
    except OSError:
        pass

def log(msg):
    line = time.strftime("%Y-%m-%d %H:%M:%S") + " " + msg
    try:
        with open(LOG, "a") as f:
            f.write(line + "\n")
    except OSError:
        pass
    print(line, flush=True)

def hist(typ, dur_ms=None):
    e = {"ts": time.strftime("%Y-%m-%d %H:%M:%S"), "type": typ}
    if dur_ms is not None:
        e["dur_ms"] = dur_ms
    state["history"].append(e)
    state["history"] = state["history"][-30:]  # 最多 30 条

def do_wipe(pressed_for_ms):
    removed = []
    for f in WIPE_FILES:
        try:
            os.remove(f)
            removed.append(os.path.basename(f))
        except FileNotFoundError:
            pass
        except OSError as e:
            log(f"删除失败 {f}: {e}")
    msg = "长按 5s 触发：已清空配网信息 " + (", ".join(removed) if removed else "（无配网文件）")
    log(msg)
    hist("wipe", pressed_for_ms)
    state["last_wipe_ms"] = int(time.time() * 1000)
    log("重启 WiFi 服务（进入 AP 配置模式）…")
    try:
        subprocess.Popen(["/etc/init.d/S30wifi", "restart"])
    except OSError as e:
        log(f"S30wifi restart 失败: {e}")

def main():
    if not os.path.exists(EV):
        log(f"错误: {EV} 不存在，按键监听不可用")
        return 1
    fd = os.open(EV, os.O_RDONLY)
    press_ts = None          # 按下时刻（monotonic）
    last_wipe = 0.0          # 上次触发时刻（monotonic）
    log("key-wipe 守护启动：长按 User Key 5s 清空 wlan 配网信息")
    write_state()
    while True:
        try:
            r, _, _ = select.select([fd], [], [], 0.2)
            if r:
                raw = os.read(fd, 24)
                if len(raw) == 24:
                    # struct input_event: timeval(64位: sec 8B + usec 8B) + type(2) + code(2) + value(4)
                    _sec, _usec, typ, code, val = struct.unpack("QQHHi", raw)
                    if typ == 1:  # EV_KEY（板上有且仅有一个按键，不区分 code）
                        if val == 1:      # 按下
                            press_ts = time.monotonic()
                            state["pressed"] = True
                            state["press_start_ms"] = int(time.time() * 1000)
                            state["pressed_for_ms"] = 0
                            hist("press")
                            write_state()
                        elif val == 0:    # 释放
                            if press_ts is not None:
                                dur = int((time.monotonic() - press_ts) * 1000)
                                hist("release", dur)
                            press_ts = None
                            state["pressed"] = False
                            state["pressed_for_ms"] = 0
                            write_state()
        except OSError as e:
            # 事件读取异常（EINVAL/EBADF 等）不崩溃：记录后重置并继续监听
            log(f"事件读取异常: {e}，继续监听")
            press_ts = None
            state["pressed"] = False
            state["pressed_for_ms"] = 0
            write_state()
        now = time.monotonic()
        if press_ts is not None:
            state["pressed_for_ms"] = int((now - press_ts) * 1000)
            write_state()
            if now - press_ts >= TRIGGER_SEC:
                if now - last_wipe >= COOLDOWN_SEC:
                    last_wipe = now
                    do_wipe(state["pressed_for_ms"])
                else:
                    log("触发被冷却拦截（距上次清空不足 60s）")
                    hist("cooldown", state["pressed_for_ms"])
                    write_state()
                press_ts = None  # 触发后忽略本次长按（需松开再按）
                state["pressed"] = False
                state["pressed_for_ms"] = 0
                write_state()

if __name__ == "__main__":
    sys.exit(main())

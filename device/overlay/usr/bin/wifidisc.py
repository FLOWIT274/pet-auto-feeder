#!/usr/bin/env python3
# wifidisc — 板子发现服务（供安卓 APP / Termux 自动跳转管理页）
# ① 每 3s 向 255.255.255.255:37777 广播 JSON：{"svc":"licheerv-admin","name":...,"ip":...,"port":8080}
# ② 收到 "LICHEERV-DISCOVER?" 探测 → 单播回同样 JSON（防 AP 隔离挡广播）
# ③ 只在 wlan0 有 IPv4 时广播；IP 变化立即补发
import fcntl
import json
import os
import socket
import struct
import sys
import time

PORT = 37777
IFACE = "wlan0"
NAME = os.uname().nodename or "licheervnano"
MAGIC = b"LICHEERV-DISCOVER?"

def get_ip():
    """ioctl SIOCGIFADDR 直取 wlan0 的 IPv4；无则 None"""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        raw = fcntl.ioctl(s.fileno(), 0x8915, struct.pack("256s", IFACE.encode()))
        return socket.inet_ntoa(raw[20:24])
    except OSError:
        return None
    finally:
        s.close()

def main():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        s.bind(("", PORT))
    except OSError as e:
        print(f"bind 失败: {e}", file=sys.stderr)
        return 1
    s.settimeout(3.0)

    last_ip = None
    last_bcast = 0.0
    while True:
        now = time.monotonic()
        ip = get_ip()
        if ip and ip != last_ip:
            last_ip = ip          # IP 变化 → 立即广播
            last_bcast = 0
        if ip and now - last_bcast >= 3.0:
            payload = json.dumps({
                "svc": "licheerv-admin",
                "name": NAME,
                "ip": ip,
                "port": 8080,
            }).encode()
            try:
                s.sendto(payload, ("255.255.255.255", PORT))
            except OSError:
                pass
            last_bcast = now
        # 收探测/收自己的包
        try:
            data, addr = s.recvfrom(512)
        except socket.timeout:
            continue
        except OSError:
            continue
        if data == MAGIC and ip:
            payload = json.dumps({
                "svc": "licheerv-admin",
                "name": NAME,
                "ip": ip,
                "port": 8080,
            }).encode()
            try:
                s.sendto(payload, (addr[0], PORT))
            except OSError:
                pass

if __name__ == "__main__":
    sys.exit(main())

# -*- coding: utf-8 -*-
"""快速脉冲观测：记录状态翻转时刻，90 秒后退出"""
import base64, hashlib, json, time
from zeroconf import Zeroconf, ServiceBrowser, ServiceStateChange, ServiceInfo
from Crypto.Cipher import AES

DEVICEKEY = "de112c32-682f-460b-b63d-64479c48b5c7"

def aes_key():
    return hashlib.md5(DEVICEKEY.encode()).digest()

def decrypt(data_b64, iv_b64):
    c = AES.new(aes_key(), AES.MODE_CBC, base64.b64decode(iv_b64))
    pt = c.decrypt(base64.b64decode(data_b64))
    return pt[:-pt[-1]]

last_state = None

def on_change(zeroconf, service_type, name, state_change):
    global last_state
    if state_change not in (ServiceStateChange.Added, ServiceStateChange.Updated):
        return
    info = ServiceInfo(service_type, name)
    try:
        if not info.request(zeroconf, 2500):
            return
    except Exception:
        return
    p = {k.decode(): v.decode() for k, v in info.properties.items()}
    raw = "".join(p.get("data%d" % i, "") for i in range(1, 5))
    if not raw or "iv" not in p:
        return
    try:
        snap = json.loads(decrypt(raw, p["iv"]))
        sws = snap.get("switches", [])
        state = "on" if any(s.get("switch") == "on" for s in sws) else "off"
        now = int(time.time() * 1000)
        if state != last_state:
            print("%d state=%s seq=%s" % (now, state, p.get("seq")), flush=True)
            last_state = state
    except Exception:
        pass

zc = Zeroconf()
ServiceBrowser(zc, "_ewelink._tcp.local.", handlers=[on_change])
time.sleep(90)
zc.close()
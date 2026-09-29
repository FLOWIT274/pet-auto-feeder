# LicheeRV Nano 后端管理系统 — Session 交接摘要

> 生成时间：2026-08-14。目标：新 session 无需历史上下文即可继续。
> 用户语言：**中文回复**。项目常识与决策记录见 `docs/knowledge.md`（ground truth，新 session 先读）。
>
> ⚠️ **2026-08-26 更正**：本文档中"狗窗口 2s / `WEB_CTRL_DOG_SECONDS=2.0`"的说法已作废——
> 实际定版是 **窗口 1.2s**，并已改为 `webd/src/control.rs` 内的**代码常量**
> `DOG_WINDOW_S=1.2` / `DOG_MIN_SCORE=0.65`（页面不可调、env 也不再可配；
> `WEB_CTRL_DOG_SECONDS`/`WEB_CTRL_DOG_MIN_SCORE` 已废弃）。以 `knowledge.md` 为准。

---

## 1. 项目总览

LicheeRV Nano（SG2002，RISC-V C906B）板载后端管理系统（webd）：
**视觉监控（推流+检测）+ 自动喂食控制（狗连续出现→扣配额→插座通电→邮件带实时照片）+ eWelink 云插座控制 + 配额管理 + 无线白名单/按键运维**。

核心规格（用户确认过的控制规则）：
- 狗连续出现 ≥2s 且窗口内无猫、**窗口内狗置信度均值 > 65%** → 触发
- 每次触发扣 **1 配额**（每日刷新，刷新时刻可配，默认 6 点）
- 触发 → 插座通电 x 秒（可配）+ 邮件带实时照片
- 狗窗口(2s)与置信度阈值(0.65)为**固定参数**：页面不可调、env 可配；其余（配额/通电时长/刷新时刻）页面可配并持久化

---

## 2. 硬件与网络环境

| 项 | 值 |
|---|---|
| 板子 | LicheeRV Nano (SG2002, RISC-V C906B, 64 位内核), buildroot |
| SSH | `root@10.86.142.1`（RNDIS USB 网卡，密码 root，sshpass，无 key） |
| 板内 wlan0 | STA 连 `YOUR_WIFI_SSID`，IP <BOARD_IP>/24，TZ UTC+0000 |
| 管理台 | webd 绑 0.0.0.0:8080 → `http://10.86.142.1:8080` |
| mDNS | avahi 常驻：`licheervnano-XXXX.local`（Android 无 .local，用 Fing 扫 IP 或连板子热点） |
| 板子热点(AP) | SSID `licheervnano-86141`，密码 `00000000`，网关 `10.141.86.1`（device_key 派生，80 端口 wificfgd 配置页） |
| eWelink 插座 | deviceid=`YOUR_SOCKET_DEVICEID`，uiid=138 四通道排插，WiFi 名 YOUR_WIFI_SSID **不可 LAN 直连，只能云控** |
| host 侧 | Ubuntu（systemd 245），USB 摄像头 /dev/video1 是唯一图像源（板子无摄像头） |

---

## 3. 组件架构与关键实现

### 3.1 ewelink-rs（板端控制 daemon）
- 源码：`/home/sbh/ws/LicheeRV/ewelink/`（api.rs / lan.rs / daemon.rs / main.rs）
- 部署：`/mnt/system/usr/bin/ewelink-rs daemon`，socket `/root/.ewelink-rs-daemon.sock`
- **固件原生脉冲**：`pulses[i]={outlet,pulse:"on",switch:"off",width:ms}` + open cmd，固件到时自动关；`pulse_all_combined` 把 switches:on 合并进**同一次** post_control（**1 次 API 调用完成全通道脉冲**）
- 全通道语义：outlet=None ⇒ 操作全部 4 通道（固件虚报 4 通道、物理 1 插座，用户决定"所有插槽全部操作"）
- 云控链路：LAN 优先（500ms 超时）→ 云；LAN 负缓存 TTL 60s，mDNS 广播到达即时清除 miss
- 触发耗时：稳态 ~1.0–1.9s（曾 4.9–5.6s）；post_control 云 RTT 1–5s 不定
- token：`/root/.ewelink-tokens.json`（at/rt/apikey/region/exp）；多端共享，最新登录踢旧；daemon 401 → 自动重登+重试一次（cloud_retry! 宏）
- 构建（**关键坑**）：只 export `CC_riscv64gc_unknown_linux_musl` + `AR_riscv64gc_unknown_linux_musl`（指向 /home/sbh/toolchains/riscv64-linux-musl-x86_64/），`cargo build --release --target riscv64gc-unknown-linux-musl`；**绝不设 CARGO_TARGET_..._LINKER**
- CLI：`ewelink-rs pulse <id> <ms>` 等；`login force` 先删 token 文件

### 3.2 webd（管理台，Rust axum）
- 源码：`/home/sbh/ws/LicheeRV/vision-server/webd/`（main.rs / control.rs / config.rs / vision.rs / push.rs / ewelink.rs / system.rs / network.rs / lan.rs 等）
- **前端内嵌**：web/index.html 用 `include_str!` 编译进二进制 → **改 HTML 必须重编译**
- 测试 37/37：`cargo test --release`（control 状态机/持久化/校验、config 校验等）
- 板端配置：`/etc/webd.env`（S90webd source 它）：`WEB_CTRL_ENABLED=1 WEB_CTRL_DOG_SECONDS=2.0 WEB_CTRL_DOG_MIN_SCORE=0.65 WEB_CTRL_QUOTA=1 WEB_CTRL_REFRESH_HOUR=6 WEB_CTRL_SOCKET_SECONDS=10 WEB_CTRL_SOCKET_ID=YOUR_SOCKET_DEVICEID`
- 状态持久化：`/mnt/data/webd_control.json`（used_today/last_quota_day/last_trigger_ms/quota/refresh_hour/socket_seconds；**不含** dog 字段）

#### 控制状态机（control.rs）
- 滑动窗口：`dog_window Vec<(ts_ms, best_dog_score)>`；触发条件 = 窗口跨度 ≥ dog_seconds **且** 均值 > dog_min_score **且** 窗口内无猫（last_cat_ms==0 时跳过猫检查；窗口保留用 `<= win_ms`）
- 有猫 → 清窗口 + 2s 抑制
- 触发 → 配额-1 → 插座脉冲（云）+ 邮件（带实时照片）；dry-run 模式只模拟不操作

#### 调试模式（dry-run）
- `AppState.dry_run: Arc<AtomicBool>`；`/api/control/debug` GET/POST 开关；`/api/control/debug/off`（POST 无 body，sendBeacon 用）
- dry-run 时：插座操作模拟、邮件标题标"[调试模式-未操作设备]"，**配额照常扣**
- **退出页面自动关**：前端 pagehide → sendBeacon debug/off；页面加载时兜底自动关闭上次遗留的 dry_run（一次性）

#### 运行时配置
- `/api/control/config` POST：`{quota?, socket_seconds?, refresh_hour?}`（校验：配额 1..=1000、通电 1..=3600、小时 0..=23）→ 校验通过才写入 + persist；**dog_seconds/dog_min_score 字段不存在（页面传了也被忽略）**
- 持久化跨重启恢复

### 3.3 管理台前端（web/index.html）
- 两个 tab：**监控**（视觉监控卡片[常开、全屏]、系统概览、网络、WS 流）与**设置**（调试模式、参数配置、按键监视、插座管理、自动喂食控制）
- 视觉全屏：vis-card requestFullscreen + `:fullscreen` CSS；移动端 `screen.orientation.lock("landscape")` + iOS fallback 提示旋转
- 自动喂食控制卡片：dog 规则实时显示、开关（启用/停用）、手动测试按钮、配额/left/下次刷新、最近触发
- 插座管理：设备列表 + 单通道/全通道控制（outlet 空=全通道）
- 按键监视卡片：/api/key/status 500ms 轮询，进度条（5s 变红）、事件历史、冷却倒计时
- 刷新保护：loadSocket 15s 节流 + skBusy 在飞守卫

### 3.4 key-wipe（长按按键清空配网）
- `/usr/bin/key-wipe.py`（守护脚本，源码也在 tpu 相关目录外单独可拷），S99keywipe 管理
- 监听 `/dev/input/event0`（gpio-keys / User Key / GPIO 510）
- **长按 5s** → 删 `/boot/wifi.ssid|wifi.pass|wifi.sta` + `/etc/wpa_supplicant.conf` → `S30wifi restart` → 无凭证自动进 AP 配置模式；60s 冷却防重复
- 状态文件 `/tmp/keywipe_state.json`（pressed/press_start_ms/pressed_for_ms/last_wipe_ms/history[]）→ webd `/api/key/status` 读
- 日志 `/var/log/keywipe.log`

### 3.5 无线白名单（防连陌生 WiFi）
- **S30wifi 已补丁**（备份 `/etc/init.d/S30wifi.prewhitelist`）：删除 `/boot/wpa_supplicant.conf` 整体复制分支；一律从 `/boot/wifi.ssid`+`wifi.pass` 重建纯净配置（`update_config=0` + 单 network + 无明文 psk 注释）
- **S98wifiguard** 常驻守卫：每 10s 四重校验（ssid 全等于白名单 / network 块≤1 / 无无-ssid 任意连接块 / 无 update_config=1），异常 → 重建 + `S30wifi restart`；日志 `/var/log/wifiguard.log`
- 出厂 wifi_fallback.sh：有凭证但 10s 扫描窗口无任何 AP → 自动切 AP 热点；无凭证 → AP + wificfgd(80)

### 3.6 视觉链路（当前已停）
- host 侧 `/tmp/opencode/vdec_tpu/super_stream.sh`：ffmpeg 采 USB /dev/video1(720p@30→10fps) → 双管道（h264 + mjpeg）经 SSH 推板端 FIFO（/tmp/live.h264.fifo, /tmp/live.jpg.fifo）→ 板端 `/mnt/data/tpu/vdec_stream`（硬解+TPU，模型 yolov8n_det_pet_person_384_640_INT8_cv181x.cvimodel）→ SHM 供 webd
- 重启方式：`nohup /tmp/opencode/vdec_tpu/super_stream.sh > ... 2>&1 &`（会自动拉起板端 vdec_stream）
- 已固化：`tpu-sdk/vdec/src/vdec_stream.c`（源码）、`tpu-sdk/vdec/bin/vdec_stream`、模型 `tpu-sdk/tpu-sdk/*.cvimodel`；TPU 运行库固件自带

---

## 4. 固件基线固化（打包自动包含）

- **打包机制**：`LicheeRV-Nano-Build/buildroot/configs/sipeed_lichee_rv_defconfig` 的 `BR2_ROOTFS_OVERLAY="board/sipeed/lichee_rv/overlay"`；下次 buildroot 构建自动合并
- **overlay 已固化**（`LicheeRV-Nano-Build/buildroot/board/sipeed/lichee_rv/overlay/`，清单见其 README-BASELINE.md）：
  - `etc/init.d/`：S30wifi（白名单版）/S90ewelink/S90webd/S98wifiguard/S99keywipe
  - `usr/bin/`：webd（1.6MB）、ewelink-rs（20.8MB）、key-wipe.py
  - `etc/webd.env`、`etc/ewelink.env`（⚠️ 含真实凭证，分发固件注意脱敏）
  - `boot/wifi.ssid|pass`（当前 YOUR_WIFI_SSID / CHANGE_ME_WIFI_PASSWORD）
- **部署脚本**：`deploy/deploy_overlay.sh [IP]`——停服务→scp 全部→起服务→健康检查；已实测通过（killall vdec_stream 防 Text file busy，host 守护会自动拉起）

---

## 5. 部署/运维速查

```bash
# 构建 webd（改 HTML 后必重编译）
cd /home/sbh/ws/LicheeRV/vision-server/webd
export CC_riscv64gc_unknown_linux_musl=/home/sbh/toolchains/riscv64-linux-musl-x86_64/bin/riscv64-unknown-linux-musl-gcc
export AR_riscv64gc_unknown_linux_musl=/home/sbh/toolchains/riscv64-linux-musl-x86_64/bin/riscv64-unknown-linux-musl-ar
cargo test --release && cargo build --release --target riscv64gc-unknown-linux-musl

# 部署 webd（停→scp→起）
sshpass -p root ssh root@10.86.142.1 '/etc/init.d/S90webd stop'
sshpass -p root scp target/riscv64gc-unknown-linux-musl/release/webd root@10.86.142.1:/mnt/system/usr/bin/webd
sshpass -p root ssh root@10.86.142.1 '/etc/init.d/S90webd start'

# ewelink-rs：先 killall -9 再 scp（Text file busy）；起前 rm -f socket；S90ewelink start
# 服务自启：S90ewelink / S90webd / S98wifiguard / S99keywipe（S30wifi 白名单版）
# 视觉恢复：nohup /tmp/opencode/vdec_tpu/super_stream.sh & 
```

---

## 6. 当前运行状态（2026-08-14 会话末）

- 板端在线：ewelink daemon、webd、wifiguard、keywipe 全部运行；wlan0=<BOARD_IP>（STA YOUR_WIFI_SSID）
- 管理台：`http://10.86.142.1:8080`（或 wlan0 IP）
- **视觉后台已按用户要求停止**（super_stream + vdec_stream 均已停；监控页无画面、自动喂食无输入=暂停；核心服务不受影响）
- 板上运行值：quota=1、socket_seconds=10、refresh_hour=6、dog_seconds=2.0（固定）、dog_min_score=0.65（固定）
- ewelink 四通道全部 OFF（设计如此）
- 测试基线：webd 37/37；5×5s 固件脉冲验收通过；长按按键/FIFO 注入全链路验证通过；wifiguard 篡改测试通过；部署脚本实测通过

---

## 7. 关键教训（坑位，别再踩）

1. **struct input_event 是 24 字节**（64 位 timeval 16B + type2 + code2 + value4）；read 缓冲 <24 必 EINVAL 崩溃；**FIFO 注入测试不校验事件大小会掩盖此 bug**——模拟测试的事件结构必须与真实设备一致
2. **busybox pkill -f 对 python 脚本进程不可靠**（SIGTERM/SIGKILL 都可能无效）；守护清理用 pidfile kill + `ps -ef | grep "[k]ey" | awk '{print $1}' | xargs kill -9` 兜底
3. **Text file busy**：scp 覆盖运行中二进制前先 killall
4. 滑动窗口 retain 用 `<= win_ms`（`<` 会丢边界样本导致永不触发）；last_cat_ms==0 时跳过猫检查（否则启动前 2s 误清窗口）
5. f32 精度：0.65f32=0.64999998，测试断言用近似（abs<1e-3）
6. 浏览器 sendBeacon 只能 POST 无 body/单 Content-Type；pagehide 比 beforeunload 可靠
7. S90 脚本里 `set -a; . env; set +a` 才能把 env 注入子进程
8. 板上无 pgrep；ps 是 busybox 版（`ps -o` 参数受限）
9. SSH 会话里起长驻后台要重定向 stdout/stderr（否则挂住 ssh 通道直到超时被杀）
10. 修改文件后用 edit 工具前先 read（file changed since read 会被拒）；多行 shell 大改优先 python 脚本 + assert

---

## 8. 待办 / 可能的方向

- [ ] 若需脱敏：打包固件前替换 overlay 内 `etc/ewelink.env`、`boot/wifi.pass` 为占位（机制不受影响）
- [ ] 视觉恢复时可考虑把 super_stream.sh 也固化成文档里的启动方式（用户此前明确**不要**服务化，保持脚本）
- [ ] 如需新 WiFi：写 /boot/wifi.ssid|pass（白名单只允许一个 SSID），重启 WiFi
- [ ] Android 访问：Fing 扫 IP / 连板子热点（licheervnano-86141 / 00000000 / 10.141.86.1:8080）

---

## 9. 用户交互偏好

- 中文回复；直接动手 + 板上实测验证 + 简洁总结
- 配置/规则类问题先确认再改；功能类直接实现并验证
- 数据/事实以 knowledge.md 与板上实测为准

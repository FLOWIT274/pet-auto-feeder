# LicheeRV Nano 基线清单（固件固化）

本目录（`buildroot/board/sipeed/lichee_rv/overlay/`）是**固件 rootfs 覆盖层**：
`BR2_ROOTFS_OVERLAY="board/sipeed/lichee_rv/overlay"`（见 `buildroot/configs/sipeed_lichee_rv_defconfig`）。
下次打包固件时，以下文件会自动合并进 rootfs，开机即生效。

## 固化文件清单

| 路径（相对 rootfs） | 内容 | 说明 |
|---|---|---|
| `etc/init.d/S20buzzer` | 蜂鸣器上电静音 | 启动早期调 `buzzer_beep.sh 0`，把 A19/GPIO499 停在低电平（高触发模块的静音态）。u-boot 已直接置低，本脚本是二次保险（防 u-boot 改动未生效时长响） |
| `etc/init.d/S30wifi` | WiFi 启动脚本 | **白名单修改版**（相对出厂版）：只连 `/boot/wifi.ssid` 指定网络；忽略 `/boot/wpa_supplicant.conf` 整体配置；生成配置强制 `update_config=0` + 单 network + 无明文口令 |
| `etc/init.d/S49ntp` | NTP 校时 | 读 `/etc/ntp.conf`；开机对齐系统时间（配额按整点刷新，时间准确是前提） |
| `etc/init.d/S90ewelink` | eWelink 控制 daemon 自启 | 依赖 S30wifi；读 `/etc/ewelink.env` 凭证；socket 就绪探测 |
| `etc/init.d/S90webd` | 管理台 Web 服务自启 | 读 `/etc/webd.env`；健康探测 5s；stop 写 `/var/run/webd.managed-off`（防 S96vision watchdog 拉起） |
| `etc/init.d/S96vision` | 板端视觉服务自启 + watchdog | 等 `/dev/video[0-3]` 就绪 → 拉起 `/mnt/data/tpu/vdec_stream_v4l2`；watchdog 每 5s：vdec 心跳冻结/摄像头缺失（DWC2 软复位→reboot 兜底）/webd 保活 |
| `etc/init.d/S97logpersist` | 持久化崩溃日志自启 | 调 `/usr/bin/crashlog.sh`，把 tmpfs 里的日志镜像到 SD（`/mnt/data/log/`），供死机后定因 |
| `usr/bin/crashlog.sh` | 崩溃日志脚本 | 镜像 `/var/log/messages` → `/mnt/data/log/messages.log` + 每 60s 一行状态（内存/swap/负载/WiFi/视觉序号）→ `status.log`；各 1MB 封顶；启动横幅判定上次是否正常关机（rcK→正常关机 / 手动 stop / 进程重启 / **无记录→⚠ 未正常关机**） |
| `etc/init.d/S98wifiguard` | 无线白名单守卫 | 每 10s 校验 wpa_supplicant.conf：ssid 白名单 / network≤1 / 无任意连接块 / 禁 update_config=1；发现异常 → 重建纯净配置 + 重启 WiFi。日志 `/var/log/wifiguard.log` |
| `etc/init.d/S99keywipe` | 长按按键清空配网 | 守护 `/usr/bin/key-wipe.py`；stop 用 pidfile kill + ps 扫描兜底（busybox pkill 对 python 不可靠） |
| `usr/bin/key-wipe.py` | 按键守护脚本 | 长按 User Key 5s → 清空 `/boot/wifi.ssid|pass|sta` + `/etc/wpa_supplicant.conf` → 重启 WiFi 进 AP 配置模式；60s 冷却；状态写 `/tmp/keywipe_state.json`（管理台"按键监视"读）；日志 `/var/log/keywipe.log` |
| `mnt/system/usr/bin/webd` | 管理台二进制 | axum，绑 0.0.0.0:8080；源码 `vision-server/webd/`；**HTML 内嵌（include_str!）**，改页面必须重编译。⚠️ 必须放 `mnt/system/usr/bin/`——`S90webd` 调的是 `/mnt/system/usr/bin/webd`（`/mnt/system` 是 rootfs 上的普通目录，不是独立分区；放 `usr/bin/` 服务起不来） |
| `mnt/system/usr/bin/ewelink-rs` | eWelink 控制 daemon | 源码 `ewelink/`；固件脉冲/全通道/云控/负缓存；token 文件 `/root/.ewelink-tokens.json`。⚠️ 同上，`S90ewelink` 调 `/mnt/system/usr/bin/ewelink-rs` |
| `usr/bin/vdec_stream_v4l2` | 视觉推理二进制（基线副本） | 与 `/mnt/data/tpu/vdec_stream_v4l2` 同一产物；板上实际运行的是 data 分区那份（S96vision 指向） |
| `usr/bin/buzzer_beep.sh` | 配额刷新蜂鸣脚本 | A19/GPIO499 **高电平触发**蜂鸣器模块（低=静音、高=响）；u-boot 起该脚即为 GPIO 输出低。节奏由 `/etc/buzzer.conf` 决定 |
| `etc/buzzer.conf` | **蜂鸣器节奏配置** | `BUZZ_COUNT`/`BUZZ_ON`/`BUZZ_GAP`/`ACTIVE_HIGH`/`RESTORE_MUX`；改完**立即生效无需重启**；试听直接跑 `buzzer_beep.sh` |
| `etc/webd.env` | webd 环境配置 | `WEB_CTRL_SOCKET_ID` 等；内容含部署环境值 |
| `etc/ewelink.env` | eWelink 凭证 | ⚠️ 含用户账号凭证，打包固件注意脱敏/替换 |
| `etc/ntp.conf` | NTP 服务器配置 | 配合 S49ntp |
| `boot/wifi.ssid` | WiFi 白名单 SSID | 当前 YOUR_WIFI_SSID |
| `boot/wifi.pass` | WiFi 白名单密码 | ⚠️ 部署环境值 |
| `boot/extlinux/extlinux.conf` | 出厂引导 | 保留原样 |

> ⚠️ **权限**：buildroot 用 `rsync -a` 合并 overlay，**原样保留文件权限**。`etc/init.d/S*` 与
> `usr/bin/*.sh|*.py` 必须带执行位（755），否则开机不会启动 / webd 调用蜂鸣脚本会失败。
> 打包前可自查：`find overlay -type f \( -name 'S*' -o -name '*.sh' -o -name '*.py' \) ! -perm -u+x`

## 配套源码/产物（不在 overlay 内）

| 位置 | 内容 |
|---|---|
| `../../../../../../ewelink/` | ewelink-rs 源码（构建：`CC_riscv64gc_unknown_linux_musl` + `AR_riscv64gc_unknown_linux_musl`，`--target riscv64gc-unknown-linux-musl`） |
| `../../../../../../vision-server/webd/` | webd 源码 + 测试（37 项） |
| `../../../../../../tpu-sdk/vdec/src/vdec_stream.c` | 视觉推理程序源码（V4L2 采集 + JPU 硬解 + TPU，`--v4l2` 模式） |
| `../../../../../../tpu-sdk/vdec/bin/vdec_stream_v4l2` | 板上运行的视觉二进制（部署到 `/mnt/data/tpu/vdec_stream_v4l2`） |
| `../../../../../../tpu-sdk/vdec/jpu_port/` | JPU 移植产物：`cv181x_jpeg.ko` / `cvi_vc_driver.ko` / `vc_shim.ko` + 补丁源码 |
| `../../../../../../tpu-sdk/tpu-sdk/yolov8n_det_pet_person_384_640_INT8_cv181x.cvimodel` | 狗/猫检测模型 |
| TPU 运行时库 `libcvikernel.so` 等 | **固件自带**（buildroot output/target/usr/bin/lib/） |

## 板端运行时依赖（非 overlay，部署时拷贝）

- `/mnt/data/tpu/vdec_stream_v4l2` + `/mnt/data/tpu/models/*.cvimodel` + `lib/ libv2/`（data 分区，`deploy/deploy_overlay.sh` 处理）
- 内核模块 `/mnt/system/ko/{soph_jpeg.ko,soph_vc_driver.ko,vc_shim.ko}`（JPU 硬解，`S00kmod` 开机 insmod）
- ~~host 侧 `super_stream.sh` 推流~~ **已退役**（现为板端 OTG 直采 + S96vision 自启；host 守护的 `pidof vdec_stream` 会误拉旧进程抢 VPU，别再启用）

## 功能速览（开机后）

1. **管理台** `http://<ip>:8080`：视觉监控（常开、全屏）、自动喂食状态机（窗口 1.2s 狗均值>65% 且无猫 → 扣 1 配额 → 插座通电 + 邮件带实时照片；窗口/阈值为代码内固定常量）、配额（默认 1/日，可配多整点刷新）、配额用尽暂停推理、调试模式（干跑）、参数配置（配额/通电时长/刷新时刻，持久化）、插座管理、按键监视
2. **配额刷新提醒**：webd 检测到配额刷新到点 → 调 `buzzer_beep.sh`（不传次数，读 `/etc/buzzer.conf`）让蜂鸣器响几下（A19 高电平触发；平时低电平静音）。
   节奏改 `/etc/buzzer.conf` 即可，例如 `BUZZ_COUNT=3 / BUZZ_ON=0.35 / BUZZ_GAP=0.30`；改完立即生效。
3. **无线白名单**：只连 wifi.ssid；无凭证 → AP 热点 `licheervnano-XXXX`（密码 00000000）+ 80 端口配置页
4. **长按 User Key 5s**：清空配网 → AP 配置模式
5. 空闲时 USB RNDIS `10.86.142.1:8080` 兜底访问；安卓 APP（UDP 37777 发现）点击直达管理页

## 部署到板（不重新打包固件时）

```bash
# 从 overlay 同步 rootfs 部分到板子（SSH），脚本见 deploy/deploy_overlay.sh
```

## 打包固件

```bash
cd LicheeRV-Nano-Build
# 按官方流程构建（buildroot defconfig: sipeed_lichee_rv_defconfig）
# overlay 自动合并；产物含 rootfs.sd / upgrade.zip
```

## 发现服务（2026-08-21 追加）
| 路径 | 内容 |
|---|---|
| `etc/init.d/S97wifidisc` | UDP 发现服务自启 |
| `usr/bin/wifidisc.py` | 每 3s 广播 `{"svc":"licheerv-admin","name","ip","port":8080}` 到 255.255.255.255:37777；应答 `LICHEERV-DISCOVER?` 探测；IP 变化立即补发 |

配套安卓 APP：`android-app/licheerv-admin.apk`（源码 `android-app/app/`，构建脚本 `android-app/build_apk.sh`）——前台服务监听+主动探测 → 通知栏点击直达管理页；开机自启。

# pet-auto-feeder — 基于 LicheeRV Nano 的宠物自动喂食监控系统

一套跑在 **Sipeed LicheeRV Nano**（Sophgo SG2002，RISC-V C906B，256MB 内存）上的边缘视觉系统：
板端摄像头实时识别 **狗 / 猫 / 人**，当判断"狗持续出现且画面无猫"时自动消耗配额、驱动智能插座为喂食器供电，
并发一封带现场照片的邮件通知。同时提供 Web 管理台、WiFi 配网、安卓发现 APP 等配套能力。

## 系统架构

```
UVC 摄像头 (OTG host)  /dev/video0  MJPEG 640x480
  │
  ▼  vdec_stream_v4l2（S96vision 开机自启 + watchdog 自愈，10fps 锁定）
  V4L2 采集 → JPU 硬解 JPEG(ioctl ~20ms) → NV12→NCHW(RVV 向量化 ~8ms)
           → TPU 推理 yolov8n (cat/dog/person, ~29ms) → NMS → 时序稳定
  │
  ▼  共享内存 /visiond_detect  (seq / ts / detections / 原始 MJPEG 帧)
  │
  ▼  webd（Rust + axum，:8080）
  ├─ WS  /api/vision/ws      检测 JSON（前端 canvas 画框）
  ├─ GET /api/vision/frame   原始 MJPEG 直返（零转码，同帧缓存）
  └─ 自动喂食状态机 control.rs
       狗连续 ≥1.2s 且均值 >65% 且窗口内无猫
         → 扣 1 配额 → ewelink pulse 插座通电 x 秒 + mailpush 邮件带照片
  │
  ▼  浏览器 / 安卓 APP
```

**判断参数（`webd/src/control.rs` 内代码常量，页面与 env 均不可改）**：窗口 `DOG_WINDOW_S=1.2`、阈值 `DOG_MIN_SCORE=0.65`。

## 仓库结构

| 目录 | 内容 | 语言 |
|---|---|---|
| `vision/` | 板端视觉主程序 `vdec_stream.c`（V4L2 + JPU 硬解 + TPU 推理）+ **JPU 内核模块移植** `jpu_port/` | C |
| `webd/` | Web 管理台（系统/视觉/插座/喂食/配额/推送/日志） | Rust (axum) |
| `ewelink/` | eWeLink 插座控制（云端 API + 局域网 mDNS 直连 + 常驻 daemon） | Rust |
| `device/` | 板端运维：init 脚本、按键清配网、WiFi 白名单守卫、发现广播、崩溃日志、部署脚本 | Shell / Python |
| `wificfgd/` | WiFi 配网门户（AP 模式下 80 端口网页填 SSID/密码） | C |
| `mailpush/` | SMTP 邮件推送（图片以 cid 内嵌正文） | Rust |
| `wxpush/` | WxPusher 微信推送（备用通知通道） | Rust |
| `android-app/` | 安卓发现 APP：UDP 37777 收到板子广播 → 通知点击直达管理页 | Java |
| `patches/` | 基线 SDK（LicheeRV-Nano-Build）的改动补丁，按主题分成 3 个 | diff |
| `docs/` | 架构决策（ADR）、设计规格、知识库、项目状态 | Markdown |
| `tools/` | 固件打包脚本（SD 卡镜像 → USB 烧录包） | Shell |

## 依赖的外部组件（本仓库不含）

| 组件 | 用途 | 获取 |
|---|---|---|
| `LicheeRV-Nano-Build` | Sipeed 基线 SDK：middleware/内核/buildroot，C 组件交叉编译与固件打包必需 | <https://github.com/sipeed/LicheeRV-Nano-Build> |
| `tpu-sdk-sg200x` | 官方 TPU SDK：`include/` + `lib/*-static.a` | <https://github.com/milkv-duo/tpu-sdk-sg200x> |
| `yolov8n_det_pet_person_384_640_INT8_cv181x.cvimodel` | 检测模型（2.9MB） | TPU SDK 自带 |
| 交叉工具链 | `riscv64-unknown-linux-musl-{gcc,g++,ar}` | Sophgo/Sipeed 工具链 |

基线 SDK 的改动以补丁形式提供（见 `patches/README.md`），不要直接把 SDK 提交进本仓库。

## 快速开始

```bash
# 0) 本地配置（含真实 IP 与路径，不进版本库）
cp config.env.example config.env && $EDITOR config.env

# 1) 主机侧组件（Rust，可直接在宿主机编译与测试）
cd webd     && cargo test --release && cargo build --release --target riscv64gc-unknown-linux-musl
cd ewelink  && cargo build --release --target riscv64gc-unknown-linux-musl
cd mailpush && cargo test --release
cd wxpush   && cargo test --release

# 2) 视觉程序（需基线 SDK；配方可逐字节复现已部署二进制）
cd vision && SDK=/path/to/LicheeRV-Nano-Build TPUSDK=/path/to/tpu-sdk ./build.sh

# 3) 配网门户（纯 C，宿主测试套件）
cd wificfgd/tests && sh run_tests.sh

# 4) 部署到板子
cp device/overlay/etc/ewelink.env.example device/overlay/etc/ewelink.env   # 填真实凭证
cp device/overlay/etc/webd.env.example    device/overlay/etc/webd.env
cp device/overlay/boot/wifi.ssid.example  device/overlay/boot/wifi.ssid
cp device/overlay/boot/wifi.pass.example  device/overlay/boot/wifi.pass
./device/deploy/deploy.sh <板子IP>
```

## 凭证与配置（重要）

**真实凭证不进版本库。** 以下文件由 `.gitignore` 排除，仓库内只保留 `*.example` 模板：

| 文件 | 内容 |
|---|---|
| `config.env` | 本地 SDK 路径、工具链路径、板子 IP |
| `ewelink/.env`、`device/overlay/etc/ewelink.env` | eWeLink APPID / AppSecret / 账号密码 |
| `device/overlay/etc/webd.env` | 插座 deviceid、配额等运行配置 |
| `device/overlay/boot/wifi.ssid`、`wifi.pass` | WiFi 白名单 SSID 与密码 |

首次使用请从对应 `*.example` 复制并填入真实值。若曾误提交过凭证，须**改密并重写历史**（`git filter-repo`）。

## 关键设计要点

- **JPU 硬解**：SG2002 硅片带 JPU（`0x0B000000`, IRQ20）但基线固件未带驱动；本项目从 `sophgo/osdrv` 的 `sg200x-dev` 分支移植了 3 个内核模块（`vision/jpu_port/`），使 JPEG 解码从软解 ~70ms 降到硬解 ~20ms。
- **全板端自包含**：摄像头经 OTG 直连板端，无宿主机推流依赖；`S96vision` 开机自启并看护视觉进程、摄像头掉线自愈（DWC2 软复位→reboot 兜底）、webd 保活。
- **零 SD 写入热路径**：采集/解码/推理/日志全程在内存（tmpfs + 共享内存），SD 卡只有低频持久化。
- **崩溃现场保留**：`device/overlay/usr/bin/crashlog.sh` 把易失日志镜像到 SD，并记录"上次是否正常关机"，用于排查整机假死。
- **配额刷新蜂鸣提醒**：A19 = GPIO499 接**高电平触发**蜂鸣器模块（低=静音）；u-boot 开机就把该脚置为 GPIO 输出低（避免 UART1_RTS 态下长响），webd 在配额刷新时拉高响两下。

更多细节见 `docs/knowledge.md`（知识库，含大量踩坑记录）与 `docs/PROJECT_STATUS.md`（当前状态与待办）。

## 许可与第三方材料

本项目自研代码以仓库内声明为准。注意以下第三方内容：

- `ewelink/docs/official-api/` 是 eWeLink（酷宅云）官方接口文档存档，版权归酷宅科技，仅供离线查阅；
- `vision/jpu_port/src/` 内的内核模块源码源自 Cvitek/Sophgo SDK（文件头保留 `Copyright (C) Cvitek Co., Ltd.`），遵循其原始许可；
- 基线 SDK 与 TPU SDK 分别遵循 Sipeed / Sophgo 的许可，本仓库仅以补丁形式记录改动。

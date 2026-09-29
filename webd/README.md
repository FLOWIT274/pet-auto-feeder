# webd — LicheeRV Nano 后台管理系统

设备端 Web 管理服务（Rust + Axum 单进程，全静态交叉编译）。
单页管理台：**系统概览 / 网络 / 视觉监控 / 日志**（视觉卡片可在页头开关，
状态记忆于 localStorage）。插座控制与推送测试的**后端 API 完整保留**（见 API 表），
仅从界面移除，后续需要时可直接接回前端。

设计文档：[2026-08-14-webd-management-system-plan.md](../docs/superpowers/specs/2026-08-14-webd-management-system-plan.md)

## 快速开始（宿主开发）

```bash
./build.sh host          # 或 cargo build --release
WEB_VISION_DEBUG=1 WEB_LISTEN=127.0.0.1:8080 ./target/release/webd
# 打开 http://127.0.0.1:8080 —— 视觉卡片以"调试模式"运行（webd 自造 SHM 模拟数据）
cargo test                # 22 用例：解析/路由/白名单/推送/SHM 布局
```

## 设备部署

```bash
./build.sh riscv                                  # 1.5MB 静态 RISC-V 二进制
sshpass -p root scp target/riscv64gc-unknown-linux-musl/release/webd \
    root@10.86.142.1:/mnt/system/usr/bin/webd
sshpass -p root scp deploy/S90webd root@10.86.142.1:/etc/init.d/S90webd
sshpass -p root ssh root@10.86.142.1 \
    'chmod +x /mnt/system/usr/bin/webd /etc/init.d/S90webd && /etc/init.d/S90webd start'
# 浏览器打开 http://<设备IP>:8080
```

- 开机自启：`S90webd`（依赖 S90ewelink 先启动，socket 集成）
- 资源实测：RSS ≈ **1.4MB**，磁盘 1.5MB（128MB 内存预算下无压力）

## API

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/` | 单页管理前端（内嵌） |
| GET | `/api/health` | 服务健康 + 版本 |
| GET | `/api/system` | CPU/内存/swap/磁盘/温度/负载/运行时间（/proc）|
| GET | `/api/network` | 网卡 IP/MAC、默认网关、WiFi 模式（wificfgd 标记）|
| GET | `/api/logs/{name}` | 日志 tail，name ∈ **ewelink / syslog / push**（白名单，最多 500 行）|
| GET | `/api/ewelink/health` | daemon 状态 + 缓存设备数 |
| GET | `/api/ewelink/devices` | devicekey 缓存设备列表 + 逐个状态 |
| POST | `/api/ewelink/on` `/off` `/status` | `{"deviceid","outlet"?}` → daemon socket 透传 |
| POST | `/api/ewelink/pulse` | `{"deviceid","ms","outlet"?}`（1~3600000ms）|
| POST | `/api/ewelink/daemon_start` | 执行 S90ewelink start 拉起 daemon |
| GET | `/api/push/health` | wxpush/mailpush bin + 凭证存在性 |
| POST | `/api/push/wxpush` | `{"content"}` → 调已部署 wxpush CLI |
| POST | `/api/push/mailpush` | `{"content","title"?,"image"?}` → mailpush CLI |
| GET | `/api/push/history` | 最近推送记录（/var/log/webd-push.log）|
| GET | `/api/vision/meta` | 视觉可用性 + 调试模式标记 |
| GET | `/api/vision/latest` | 最新检测 JSON（SHM，visiond 布局）|
| GET | `/api/vision/frame` | 最新 JPEG 帧 |
| WS | `/api/vision/ws` | 每帧检测推送（500ms 轮询 SHM sequence）|

## 环境变量（全部可选）

| 变量 | 默认 | 说明 |
|------|------|------|
| `WEB_LISTEN` | `0.0.0.0:8080` | 监听地址 |
| `WEB_HOME` | `$HOME`（设备 /root）| ewelink socket/devicekey 缓存目录 |
| `WEB_BOOT_DIR` | `/boot` | WiFi 标记文件目录 |
| `WEB_WXPUSH_BIN` | `/mnt/system/usr/bin/wxpush` | wxpush 路径 |
| `WEB_MAILPUSH_BIN` | `/mnt/system/usr/bin/mailpush` | mailpush 路径 |
| `WEB_PUSH_LOG` | `/var/log/webd-push.log` | 推送历史 |
| `WEB_SYSLOG` | `/var/log/messages` | 系统日志源 |
| `WEB_WXPUSHER_CONF` / `WEB_MAIL_CONF` | `/etc/wxpusher.env` / `/etc/mail.env` | 凭证健康检查 |
| `WEB_EWELINK_INIT` | `/etc/init.d/S90ewelink` | daemon 拉起脚本 |
| `WEB_VISION_DEBUG` | 未设置 | 视觉调试模式（自造 SHM 模拟数据，仅开发机）|

## 集成约定

- **ewelink**：Unix socket 客户端（ADR 0004：一行指令一行 JSON），webd 零凭证
- **visiond**：SHM `/visiond_detect` 只读（布局见 `src/vision.rs` 注释，与
  vision-server 设计文档 4.3 一致）；visiond 未运行 → 全部 vision API 503 降级
- **wxpush/mailpush**：spawn 已部署 CLI，退出码 0/1/2/3/4 原样回显；限频由调用方负责
- 视觉检测类别：0=cat 1=dog（设计文档约定）

## 已知边界

- 插座与设备不同网段时 mDNS 直连失败，`status` 错误原样透传（UI 如实显示）
- `/etc/wxpusher.env` 若缺失，微信推送面板显示"无配置"（需部署 SPT 凭证）
- 视觉卡片需 visiond 运行（当前 SHM 未写入时显示"离线"）
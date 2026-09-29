# webd 后台管理系统 — 补全方案

> 2026-08-14
> 项目：LicheeRV Nano (SG2002) + 视觉 + 智能插座 + 推送
> 关联：2026-07-30-vision-server-design.md（webd 设计）、ADR 0004（ewelink daemon + Unix socket）

---

## 1. 现状盘点（项目阅读结论）

设备端已就绪并实机验证的组件：

| 组件 | 形态 | 状态 | 对外接口 |
|------|------|------|----------|
| `ewelink-rs` | Rust daemon + CLI，静态 20MB | ✅ 部署 `/mnt/system/usr/bin/` + S90ewelink | Unix socket `~/.ewelink-rs-daemon.sock`，一行指令一行 JSON |
| `wxpush` | Rust CLI，静态 8.9MB | ✅ 实机推送成功 | CLI：`wxpush <内容>`，凭证 `/etc/wxpusher.env` |
| `mailpush` | Rust CLI，静态 5.5MB | ✅ 实机推送成功（图片内嵌） | CLI：`mailpush <内容> [--title] [--image]`，凭证 `/etc/mail.env` |
| `wificfgd` | C 静态二进制 + S30wifi 改造 | ✅ 实机验证 | AP 窗口期 80 端口配置页（内嵌 HTML） |
| 视觉检测 | visiond 设计文档 + 设备端 vdec_tpu30 二进制（22.5fps 实机） | ⚠️ **仓库内无源代码** | 设计：SHM `/visiond_detect` + `/visiond_ready_sem` |
| OTA / Boot A/B / swapmon / envtool | buildroot 层 | ✅ | 脚本 + 标记文件 |
| **webd**（Rust Web 服务层） | — | ❌ **完全未实现** | 设计文档要求：REST + WS + 预留 eWelink 桥接 |

**结论**：系统"手脚"齐全（控制、推送、视觉流水线），但**没有统一的管理入口**。
设计文档里定义的 webd（Axum HTTP 服务，0.0.0.0:8080）是整个后台管理系统的核心载体，目前为零。

---

## 2. 目标与范围

### 2.1 目标

实现 **webd：设备端单进程 Web 管理服务**（Rust + Axum），把以下能力统一进一个
局域网可访问的管理控制台（`http://<设备IP>:8080`）：

1. **系统概览** — CPU / 内存 / swap / 磁盘 / 运行时间 / 网络 IP / 版本
2. **视觉监控** — 最新帧 JPEG + 检测结果 + WS 实时推送（visiond 就绪后）
3. **插座控制** — eWelink 设备列表 / on / off / pulse / 状态（经 daemon socket）
4. **推送测试** — 手动触发 wxpush / mailpush，显示结果与凭证状态
5. **WiFi 状态** — STA/AP 模式、SSID、IP、信号
6. **日志查看** — daemon 日志、推送日志、系统日志（tail）
7. **（v2 可选）** 定时喂食计划、OTA 触发、配置热加载

### 2.2 非目标（v1 明确不做）

- 不做用户/多会话管理（单用户本地网络服务，沿用设计文档结论）
- 不做 HTTPS（局域网明文，v2 可选）
- 不实现 visiond 本身（那是 C 子项目，本方案只在 visiond 就绪后对接其 SHM）
- 不重写 wificfgd / wxpush / mailpush / ewelink（全部走既有接口调用）

---

## 3. 总体架构

```
浏览器 (局域网)
   │  http://<dev>:8080  (单页管理台)
   ▼
┌──────────────────────────── webd (Rust + Axum, 单进程) ────────────────────────────┐
│  GET /                  → 内嵌单页前端（无构建，原生 JS）                            │
│  GET /api/system        → /proc 解析 + sysinfo（CPU/内存/swap/磁盘/uptime）          │
│  GET /api/network       → 网卡 IP/MAC、STA/AP 模式、SSID、信号                       │
│  GET /api/vision/whoami → visiond 就绪探测（SHM 存在性），未就绪 503                 │
│  GET /api/vision/latest → SHM 读最新检测 + 帧（visiond 就绪后）                     │
│  GET /api/vision/frame  → image/jpeg 最新帧                                         │
│  WS  /api/vision/ws     → 每帧检测结果推送（轮询 SHM sequence_num）                 │
│  GET /api/ewelink/devices → daemon socket: 设备列表 + 快照状态                       │
│  POST /api/ewelink/{on,off,pulse,status} → daemon socket 指令透传                   │
│  POST /api/push/{wxpush,mailpush} → 调已部署 CLI，返回退出码 + 输出                  │
│  GET  /api/push/health  → 凭证存在性 + 最近结果                                    │
│  GET  /api/logs/{name}?lines=N → tail 日志文件                                      │
└──────────────────────────────┬─────────────────────────────────────────────────────┘
                               │
          ┌────────────────────┼───────────────────────────┬──────────────┐
          ▼                    ▼                           ▼              ▼
   ewelink daemon        visiond (SHM, 就绪后)      wxpush CLI      mailpush CLI
   (~/.ewelink-rs-      (/visiond_detect)          (/mnt/system/   (/mnt/system/
    daemon.sock)                                    usr/bin/)       usr/bin/)
```

### 3.1 关键决策（沿用既有结论 + 新增）

| 决策点 | 选择 | 理由 |
|--------|------|------|
| Web 框架 | axum 0.8 + tokio current_thread | 设计文档既定；单核 C906B 省内存 |
| 前端 | **单 HTML 内嵌二进制**（原生 JS + fetch，无构建链） | 设备端无 node；单文件部署；十几个 API 足够 |
| 静态资源 | `include_str!` / `include_bytes!` 打进二进制 | 与 ewelink/wxpush 全静态风格一致，无运行期文件 |
| eWelink 集成 | 只做 daemon socket 客户端（不透传云端 API） | ADR 0004 既定；免凭证、免端口、进程隔离 |
| 凭证 | webd 自身零凭证；推送/插座凭证都在既有环境文件里 | 最小攻击面 |
| 认证 | v1 无认证（局域网），预留 `X-Manage-Token` 头检查开关 | 设计文档：单用户本地服务 |
| 交叉编译 | riscv64gc-unknown-linux-musl + rust-lld + crt-static | ewelink 已验证的完整工具链（~/.cargo/config.toml） |

### 3.2 ewelink daemon socket 协议（webd 客户端封装）

已确认协议（`ewelink/src/daemon.rs`）：**一行指令 → 一行 JSON**（`\n` 结尾）。

```
ping                      → {"ok":true,"mode":...,"version":...}
on <deviceid> [outlet]    → {"ok":true,"sent":...,"confirm_msg":...}
off <deviceid> [outlet]
pulse <deviceid> <ms> [outlet]  → {"ok":true,"target_ms":...,"wall_ms":...}
status <deviceid>         → {"ok":true,"deviceid":...,"switches":[...]}
quit                      → {"ok":true}
```

- socket 路径 `~/.ewelink-rs-daemon.sock`（webd 以 root 运行 → `/root/`）
- 连接失败 = daemon 未运行 → API 返回 `503 {"error":"daemon_not_running"}`，
  前端给出"尝试拉起：`/etc/init.d/S90ewelink start`"提示（或 webd 直接 spawn 该脚本）
- **已知阻塞点透传**：插座与设备不同局域网时 `status` 会返回 error（mDNS 发现失败）。
  UI 需如实展示错误，不误报。云端 API 控制（api.rs）v1 不纳入 webd，避免凭证与配额问题。

### 3.3 vision 对接（visiond 就绪前先做 stub）

- 按设计文档 SHM 布局定义 Rust 结构（`sequence_num` / `detections[10]` / `frame_jpeg`）
- `shm_open("/visiond_detect")` 失败 → 全部 vision API 返回 503 + `visiond_offline`，
  前端显示"视觉服务未运行"占位卡片（不阻塞其他功能）
- visiond 就绪后无需改动 webd 代码（协议已定型），直接出图出检测

### 3.4 推送集成

```
POST /api/push/wxpush   {"content":"..."}   → spawn /mnt/system/usr/bin/wxpush <content>
POST /api/push/mailpush {"content":"...","title":"...","image":null}
                                            → spawn /mnt/system/usr/bin/mailpush ...
响应：{"exit":0,"stdout":"wxpush: 已发送","stderr":""}
```
- `GET /api/push/health`：检查 `/etc/wxpusher.env`、`/etc/mail.env` 存在性与关键字段
- 推送是低频操作（ClawBot 限 10 条/24h），webd 不做冷却，UI 提示即可
- 进程执行用 `tokio::process::Command`，超时 30s（mailpush SMTP 可能慢）

### 3.5 系统信息

- 全部走 `/proc`（meminfo / loadavg / uptime / net/dev）+ `statvfs`（/ 与 /mnt/data）
- 温度：`/sys/class/thermal/thermal_zone0/temp`（存在才返回）
- 进程列表不做（top 类功能留给 SSH，v1 只做概览数字）

---

## 4. 前端设计（单页，无构建）

单文件 `index.html`（~300 行原生 JS + CSS，深色卡片式）：

| 卡片区 | 内容 |
|--------|------|
| 系统概览 | CPU 占用 / 内存 / swap / 根分区 / 运行时间 / IP，5s 自动刷新 |
| 视觉监控 | 最新帧 `<img>` + 检测框列表 + 猫/狗计数；WS 连接失败显示离线 |
| 插座控制 | 设备列表（deviceid + 状态灯）+ on/off 按钮 + pulse（毫秒输入）+ 最近响应 |
| 推送测试 | 两个表单（微信 / 邮件）+ 凭证健康指示 + 结果输出框 |
| 网络 | STA 模式：SSID / IP / 信号；当前模式来源 `/api/network` |
| 日志 | 下拉选日志源 → 最近 N 行（pre 块，自动刷新开关） |

交互纯 fetch + 少量 DOM 更新，无框架无依赖，浏览器直接可用。
WS 仅视觉卡片使用，其余全部 REST。

---

## 5. 分阶段实施计划

### 阶段 1：webd 骨架 + 系统/网络/日志 API + 前端外壳（可独立交付）
- 新建 `vision-server/webd/`（Cargo 工程，axum + tokio + serde）
- `/api/system` `/api/network` `/api/logs/*` + 单页前端外壳 + 系统概览卡片
- 宿主验证：`cargo test`（/proc 解析单测）+ 宿主 `cargo run` 冒烟
- 验收：浏览器可打开管理台，系统数字正确

### 阶段 2：ewelink 控制面板
- socket 客户端模块（connect + 一行指令 + 读一行 JSON，超时 10s）
- `/api/ewelink/*` + 前端插座卡片
- 验收：daemon 运行时设备状态/开关/pulse 全链路；daemon 未运行返回 503 且前端提示

### 阶段 3：推送测试面板
- `/api/push/*` + `/api/push/health` + 前端表单
- 验收：实机各发一条微信/邮件，退出码与输出正确回显

### 阶段 4：vision 对接（stub → 实接）
- SHM 读取模块 + WS 推送任务（按设计文档协议）
- 验收：visiond 未运行 503 降级；visiond 部署后（后续 C 子项目）出图 + 实时检测

### 阶段 5：交叉编译 + 设备部署
- 复用 ewelink 工具链参数编译 riscv64gc-unknown-linux-musl 静态二进制
- 部署 `/mnt/system/usr/bin/webd` + `/etc/init.d/S90webd`（依赖 S90ewelink 之后启动）
- 实机验收：8080 全功能走查，内存占用实测（预算 ≤ 15MB RSS）

### 阶段 6（v2，另立方案）
- 定时喂食计划（每日 pulse 排程，webd 内 tokio 定时器 + JSON 配置持久化）
- OTA 触发面板、交换区/swapmon 状态、HTTPS、Token 认证

---

## 6. 风险与依赖

| 风险 | 影响 | 对策 |
|------|------|------|
| visiond 源码不在仓库（仅设计文档 + 设备端二进制） | 阶段 4 只能 stub | 先完成 1-3/5，vision 对接留到 visiond 工程化时同步做 |
| 插座与设备不同网段 → lan 直连失败 | 控制面板状态不可用 | UI 如实报错；后续可评估把云 API 控制纳入 webd（需凭证入 webd 配置） |
| ClawBot 推送限 10 条/24h | 频繁测试会触发 ret=-2 | 前端提示限频；webd 不做自动重试 |
| 128MB 内存预算 | axum + tokio 静态链约 10-15MB | 与 ewelink 同法 crt-static；运行时 current_thread；实测 RSS 后调优 |
| 设备 C906B 编译慢 | 迭代效率 | 宿主 `cargo run` 开发，交叉编译仅最终部署前做（复用已验证工具链） |

---

## 7. 待确认决策（实施前）

1. **范围**：v1 是否包含视觉卡片（stub 形态先行）还是先做 系统/插座/推送/日志 四块？
2. **前端形态**：内嵌单页原生 JS（推荐）vs 允许引入 Vue/React（需构建链，部署复杂度上升）
3. **认证**：v1 无认证（推荐，局域网单用户）vs 简单 Token 开关
4. **实施入口**：确认后从阶段 1 开始编码，阶段 2/3/5 连续推进，实机验证在编译环境就绪后进行

---

## 8. 实施状态（2026-08-14 更新）

已确认决策：**范围 = 系统/网络/插座/推送/日志 + 视觉卡片（调试模式先行）**；无认证；立即实施。

| 阶段 | 状态 | 说明 |
|------|------|------|
| 1 骨架 + 系统/网络/日志 API + 前端 | ✅ | 22 用例全过，宿主冒烟通过 |
| 2 eWelink 控制面板 | ✅ | socket 客户端 + 设备列表/on/off/status/pulse/daemon_start；实机验证 socket 透传 |
| 3 推送测试面板 | ✅ | wxpush/mailpush 调用 + 凭证健康 + 历史；实机真发邮件 exit 0 |
| 4 vision 对接（stub + 调试模式） | ✅ | SHM 只读实现 + WS 推送 + `WEB_VISION_DEBUG` 自造数据；visiond 未运行 503 降级 |
| 5 交叉编译 + 部署 | ✅ | 1.5MB 静态 RISC-V；`/mnt/system/usr/bin/webd` + S90webd；实机 RSS 1.4MB |
| 6 v2（定时喂食/OTA/HTTPS/Token） | ⏳ | 另立方案 |

实机遗留（非 webd 问题）：`/etc/wxpusher.env` 凭证缺失（重刷丢失，需补部署 SPT）；
devicekey 缓存为空（插座面板显示"无设备缓存"，受既有不同网段阻塞点影响）。

**UI 调整（2026-08-14）**：界面收敛为 系统概览/网络/日志 + 视觉卡片（页头开关显隐，
localStorage 记忆）；插座控制与推送测试卡片从界面移除，后端 API 逻辑完整保留
（`/api/ewelink/*`、`/api/push/*` 原样可用），后续需要时仅接回前端即可。

代码仓库：`vision-server/webd/`（README 含部署与 API 文档）。
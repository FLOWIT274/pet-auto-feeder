# ewelink-rs

基于 Rust 的 **eWeLink（易微联 / 酷宅云）开放平台 v2 API** 客户端，
用于登录、查询设备列表、控制智能插座开关，并通过 WebSocket 长连接实时监听设备状态。

协议实现严格依据官方文档（已存档在 `docs/official-api/`，在线版：
<https://coolkit-technologies.github.io/eWeLink-API/#/zh-cmn/平台概述>）。

## 前提条件

1. **APPID + App Secret**：向酷宅商务申请（开放平台「成为开发者」流程），两者配套使用；
2. **eWeLink 账号**：设备已通过易微联 APP（或官方渠道）添加到该账号下；
3. **Rust 工具链**：`rustup` / `cargo`。

## 快速开始

```bash
# 环境变量（换成你自己的值；Windows PowerShell 用 $env:XXX=... 或 set XXX=...）
export EWELINK_APPID="你的APPID"
export EWELINK_APPSECRET="你的AppSecret"
export EWELINK_ACCOUNT="+8613800138000"     # 账号（手机号写完整带区号的形式亦可）
export EWELINK_PASSWORD="你的密码"
export EWELINK_COUNTRY_CODE="+86"           # 默认 +86
export EWELINK_REGION="as"                  # cn/as/us/eu，默认 as；10004 会自动重定向

cargo run -- login                  # 登录，输出 region / apikey / at / rt
cargo run -- list                   # 列出账号下所有设备（deviceid、在线状态、uiid）
cargo run -- on   <deviceid>        # 打开插座（多通道排插会全部插位一起开）
cargo run -- on   <deviceid> <n>    # 只开第 n 个插位（0 起）
cargo run -- off  <deviceid>        # 关闭插座
cargo run -- off  <deviceid> <n>    # 只关第 n 个插位
cargo run -- status <deviceid>      # 查询插座状态
cargo run -- ws                     # WebSocket 长连接监听实时状态（Ctrl+C 退出）
cargo run -- ws <deviceid> on       # 长连接下演示通过 WS 下发开/关指令

# ---- 局域网直连（推荐：同局域网内控制不消耗云端 API 配额）----
cargo run -- lan devices            # mDNS 发现局域网内 eWeLink 设备并显示实时状态
cargo run -- lan status <deviceid>  # 读取设备实时状态（设备推送的加密快照）
cargo run -- lan on   <deviceid> [n]   # 局域网开启第 n 个插位；不指定 n 则全部插位同时开
cargo run -- lan off  <deviceid> [n]   # 局域网关闭第 n 个插位；不指定 n 则全部插位同时关
```

> `deviceid` 从 `list` 命令输出中获取。
>
> 局域网控制首次运行需要联网执行一次 `login`（用于从云端获取设备的 `devicekey`，
> 之后缓存到 `~/.ewelink-rs-devicekeys.json`，可以完全离线控制）。

### 局域网直连（LAN 模式，推荐主控通道）

同一局域网内的设备通过 mDNS 广播自身（服务类型 `_ewelink._tcp.local.`），
可直接经 `HTTP 8081` 端口控制，**全程不走云端 API，不受免费账号调用配额限制**。

### 发现设备（mDNS）

```bash
ewelink-rs lan devices
# 共 1 台设备（局域网直连）:
#   eWeLink_YOUR_SOCKET_DEVICEID._ewelink._tcp.local.  deviceid=YOUR_SOCKET_DEVICEID  192.168.0.108:8081
#       switches: [{"outlet":0,"switch":"off"},...]
```

### 实时状态（加密快照）

设备定期（以及状态变化时）通过 mDNS TXT 记录推送加密状态快照：
TXT 的 `data1..data4` 拼接后与 `iv` 一起用 `AES-128-CBC`（密钥 = `MD5(devicekey)`）
解密即得当前 `switches` 等参数。**这是免轮询的实时状态来源**，与云端一致。

```bash
ewelink-rs lan status YOUR_SOCKET_DEVICEID
```

### 控制指令

```bash
ewelink-rs lan on YOUR_SOCKET_DEVICEID          # 全部插位同时开启
ewelink-rs lan on YOUR_SOCKET_DEVICEID 0        # 只开 outlet 0（兼容旧用法）
ewelink-rs lan off YOUR_SOCKET_DEVICEID         # 全部插位同时关闭
```

### 常驻 mDNS 守护（低延迟）

进程内启动后台 mDNS 监听线程，持续接收设备主动广播并维护设备缓存
（**缓存条目 5 分钟有效，过期自动重新解析**）。控制流程：

1. 缓存命中 → 直接拿 IP/端口发 HTTP 指令（跳过 mDNS 解析等待）；
2. 设备收到指令后状态变化，会主动广播新快照（<1s）；
3. 回读比对确认，无需重新解析。

实测单次 `lan on/off` 完整闭环 **≈0.85–2.8s**（原 mDNS 重解析方案 7–11s，
瓶颈在等待设备响应查询；控制 HTTP 本身仅 ~10ms）。

### 定时脉冲（pulse，设备精确通电时长）

```bash
ewelink-rs lan pulse YOUR_SOCKET_DEVICEID 8000      # 开启 → 保持 8000ms → 自动关闭
ewelink-rs lan pulse YOUR_SOCKET_DEVICEID 5000 0    # 可指定插位（默认全部插位）
```

守护进程收到指令后：发 `on` → 进程内 `tokio::sleep(ms)` → 自动发 `off`，
**全程不出进程**，免去脚本调度误差与跨进程解析延迟。

实测（独立观测器按 mDNS 状态翻转计时）：

- 守护报告 wall（on→off 命令返回）≈ **目标 + 0.3s**；
- **设备实际通电 ≈ 目标 + 1.1s**（实测 8000ms 目标 → 9112ms），
  波动 ±0.3s，主要残余是设备 mDNS 状态广播本身的时序延迟（固件行为）。

相比脚本模式（`sleep 30` + 两次独立进程，实测 +2.6s 且波动 ±1.5s），
pulse 更贴近目标值、时序稳定，适合需要可预期通电时长的场景。

### 常驻守护进程（daemon，推荐用于定时/连续控制）

`lan on/off/status` 会自动检查常驻守护是否在监听；不在则自动拉起
（`ewelink-rs daemon` 可手动前台运行，日志写入 `~/.ewelink-rs-daemon.log`）。
守护进程内常驻 mDNS 监听线程与设备热缓存（5 分钟有效），连续指令共享状态：

| 场景 | 命令耗时 |
|---|---|
| 首次（拉起守护 + 预热） | ~3.8s |
| 后续 on/off（热缓存） | **~0.3s** |
| status（热缓存） | **~7ms** |

实测 `名义 30s 通电`（5 轮，独立观测器计时）：**31–34s，平均 32.6s**
（相比每命令单进程直连的 31–36s 平均 33.4s 收窄）。残余偏差来自设备
mDNS 状态广播本身的延迟（0.3–2s 抖动，设备侧行为，无法压缩）；
命令往返本身的偏差已压到 ~0.3s。

> 两级确认：先认设备返回 `error:0`，再比对广播快照中的插位状态；
> 快照暂不可得时降级提示（不误报失败）。

> 实测该硬件实际为**单槽位**（固件误用 4 通道排插模板），因此默认不带插位参数时
> 会向全部 4 个 outlet 同时下发指令，行为等价于单通道插座。
>
> 控制走「发送成功（error:0）+ 回读确认」两级确认：下发后等 1.2s 重新解析
> mDNS 快照比对插位状态；快照暂不可得时降级提示（不误报失败）。

请求体与 eWeLink 局域网协议一致：`POST /zeroconf/{command}`，
`data` 字段用 AES-128-CBC+PKCS7 加密（密钥同上），其余字段（`sequence`/`deviceid`/
`selfApikey`/`encrypt`/`iv`）明文。实测 uiid=138 四通道插座**只认 `switches` 命令**
（`switch`/`getState` 均返回 `error=400`）。

### 踩坑：Header 名必须大写

该机型固件（openresty + 自写 HTTP 解析器）对请求头名称**大小写敏感**：
reqwest/hyper 输出的小写头名（`host:`、`content-type:`、`connection:`）会被
设备直接关闭连接（`connection closed before message completed`）；
改为大写（`Host:`、`Content-Type:`、`Connection:`）即返回 `200 error:0`。
因此本项目局域网控制用手写 HTTP/1.1（tokio TcpStream）而非 reqwest。

## 认证：自动 OAuth2.0 回退

标准 v2 登录接口 `POST /v2/user/login` 需要**付费 APPID** 的接口权限。
若您的 APPID 为免费档，登录接口会返回 `error=407 the path of request is not allowed`，
客户端会自动切换到官方 **OAuth2.0 授权码流程**：

1. `POST https://apia.coolkit.cn/v2/user/oauth/code`（账号密码 + 签名 -> 授权码 `code`，30s 有效）
2. `POST {region-apia}/v2/user/oauth/token`（`code` -> `accessToken` / `refreshToken`）
3. `GET /v2/family` 获取用户 `apikey`（OAuth 响应不含 apikey）

整个过程对用户透明，`login` 命令的两行提示即可看出走了哪条路。

### 代理支持（可选）

WebSocket 长连接支持经 HTTP 代理（CONNECT 隧道）建立，方便受限网络环境：

```bash
export HTTPS_PROXY="http://127.0.0.1:7897"   # 或 https_proxy / ALL_PROXY
```

不设置则直连。HTTP 接口（API 域名）始终直连，不受影响。

## 协议要点（官方 v2）

### 域名

| 区域 | HTTP 接口 | WS 分配服务 |
| ---- | --------- | ----------- |
| 中国 | `https://cn-apia.coolkit.cn` | `https://cn-dispa.coolkit.cn/dispatch/app` |
| 亚洲 | `https://as-apia.coolkit.cc` | `https://as-dispa.coolkit.cc/dispatch/app` |
| 美洲 | `https://us-apia.coolkit.cc` | `https://us-dispa.coolkit.cc/dispatch/app` |
| 欧洲 | `https://eu-apia.coolkit.cc` | `https://eu-dispa.coolkit.cc/dispatch/app` |

### 请求头

| Header | 说明 |
| ------ | ---- |
| `X-CK-Appid` | APPID（用户类接口必填） |
| `X-CK-Nonce` | 8 位字母数字随机串 |
| `Authorization` | 登录前 `Sign {base64(HMAC-SHA256(appSecret, body))}`；登录后 `Bearer {at}` |
| `Content-Type` | `application/json` |

### 关键接口

- 登录 `POST /v2/user/login`（body 原文做签名；`error=10004` 表示区域不对，应按 `data.region` 换区重试）
- OAuth2.0 授权码 `POST /v2/user/oauth/code`（免费 APPID 用，见上）
- 刷新 `POST /v2/user/refresh`（body `{"rt": ...}`）
- 设备列表 `GET /v2/device/thing?num=0`
- 获取状态 `GET /v2/device/thing/status?type=1&id={deviceid}`
- 更新状态 `POST /v2/device/thing/status`（body `{"type":1,"id":"{deviceid}","params":...}`）

### WebSocket 长连接

1. `GET /dispatch/app` 拿到长连接服务器 `domain:port`；
2. 连接 `wss://{domain}:{port}/api/ws`；
3. 发送 `userOnline` 握手（`version:8, at, apikey, appid, nonce, sequence`）；
4. 控制：`{"action":"update", "deviceid", "apikey", "userAgent":"app", "sequence", "params":{...}}`；
5. 查询：`{"action":"query", "deviceid", "apikey", "params":["switch"], "userAgent":"app", "sequence"}`；
6. 按握手返回的 `config.hbInterval` 发心跳（本项目默认 60s Ping）。

## 智能插座常用参数

| 参数 | 说明 |
| ---- | ---- |
| `switch` | `"on"` / `"off"`（单通道插座） |
| `switches` | `[{"outlet":0,"switch":"on"}, ...]`（多通道排插，按 outlet 控制各插位） |
| `power` / `voltage` / `current` | 功率(W)/电压(V)/电流(A)，取决于固件与 UIID |

具体协议由设备 `extra.uiid` 决定，详细参数见 `docs/official-api/UIID协议.md`。
本项目 `on/off` 会自动探测设备参数：含 `switches` 数组则按多通道处理，否则用 `switch` 字段。

> 实测机型为 **uiid=138「单通道插座_支持 2.4G 轻智能」**，但固件实际返回 4 个插位的
> `switches` 数组，控制用 `{"switches":[{"outlet":0,"switch":"on"}]}` 格式。

## 常见错误码

| 错误码 | 含义 |
| ------ | ---- |
| 0 | 成功 |
| 400 | 参数错误 |
| 401 | access token 无效（如被其他登录顶掉） |
| 402 | access token 过期（用 `refresh` 刷新或重新登录） |
| 403 | 无权限/接口不存在 |
| 405 | 方法不允许 |
| 407 | APPID 无该接口权限（免费 APPID 登录就走 OAuth2.0） |
| 10004 | 账号不在当前区域（自动重定向） |
| 30022 | 设备离线，操作失败 |

## 注意事项

- 单个 IP 对相同接口调用间隔尽量 ≥500ms，5 分钟内不超过 300 次；
- WS 不应频繁重复 `userOnline`（防封）——长连接建立后保持住即可；
- 官方建议：设备状态变化通过长连接被动接收，不要轮询 HTTP 接口；
- AT 默认 30 天失效，RT 60 天，可用 `refresh` 命令续期；
- 若遇到「TCP 能连通但 TLS 握手超时」且本机启用了 Clash 等 fake-IP 模式代理
  （DNS 解析返回 198.18.0.x 段），请关闭代理或设置 `HTTPS_PROXY` 走隧道——见「代理支持」。

## 项目结构

```
src/
  main.rs    CLI 入口与命令分发
  api.rs     HTTP API 客户端（签名/登录/OAuth2.0/设备列表/开关/状态）
  lan.rs     局域网直连（mDNS 发现/快照解密/zeroconf 控制、devicekey 缓存）
  ws.rs      WebSocket 长连接（代理隧道/直连、握手/控制/监听、心跳）
  models.rs  与官方 JSON 对应的 serde 模型
  sign.rs    HMAC-SHA256 签名、nonce、时间戳（含官方 Demo 校验测试）
examples/
  tls_probe.rs  直连 / 代理隧道分步诊断（TCP -> TLS -> Upgrade）
docs/
  official-api/ 官方文档存档（开发文档_v2、接口中心_v2、接口清单_v2、UIID协议、OAuth2.0 等）
```
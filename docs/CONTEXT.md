# vision-server 项目术语

## eWelink 控制（ewelink-rs）

ewelink-rs
: eWeLink（酷宅云）开放平台 v2 API 的 Rust 客户端（CLI + 常驻 daemon）。局域网 zeroconf 直连插座（mDNS 发现 + AES-128-CBC 快照解密 + UDP 控制），云端 API（login/list/on/off）做设备发现与 devicekey 拉取。终端要整合的"实际控制端"。
_Avoid_: webd（与 vision-server 的 Rust webd 服务重名；ewelink-rs 是控制端，webd 是 Web 服务层）

ewelink daemon
: ewelink-rs 的常驻进程（`ewelink-rs daemon`），进程内持有 mDNS 后台线程与设备热缓存，Unix socket（`$HOME/.ewelink-rs-daemon.sock`）一行指令一行 JSON 响应；CLI 薄壳自动拉起守护（ensure_daemon）。控制指令（on/off/pulse/status）经 socket 分发，局域网直连不经云端。

devicekey 缓存
: 设备直连所需密钥（deviceid → devicekey，云端 `/v2/device/thing` 拉取，`$HOME/.ewelink-rs-devicekeys.json`）。解密快照用 `AES-128-CBC + PKCS7`，`key = MD5(devicekey)`，iv 随机 16 字节。控制前必须存在（`ewelink-rs login` 拉取）。

pulse（脉冲）
: 定时开→保持 ms 毫秒→自动关的完整流程，全程在 daemon 进程内计时（上限 1h）。如给猫喂食器供电 N 秒。

## WiFi 首次配置

wificfgd
: 设备的 WiFi 配置服务守护进程（C 语言，独立编译部署/固化 overlay）。在"扫描不到可用 WiFi"的配置窗口期于 AP 模式下监听 80 端口，提供输入 SSID/密码的网页；提交后将凭证写入 /boot 标记文件并重启切回 STA 模式。
_Avoid_: webd（与 vision-server 废弃的 Rust webd 重名）

配置窗口期
: 设备判定当前无可用 WiFi（扫描窗口内 `wpa_cli scan_results` 无网络）而临时开启 AP 模式的时段——从切 AP 起到用户提交配置 reboot 为止。窗口期内 wificfgd 是唯一对外服务。

## 微信推送（wxpush）

wxpush
: 设备端微信推送工具（Rust，静态 8.9MB）。用 WxPusher SPT 极简接口向微信/App 发文本通知。无状态幂等，去抖/冷却由调用方（visiond）负责，wxpush 自身不做。
_Avoid_: pushplus、Server酱（推送频率/实名限制多；本项目已定 WxPusher）

SPT
: WxPusher 极简推送令牌（`SPT_` 开头，GET `/api/send/message/{SPT}/{内容}`）。获取方式=扫官方文档 spt.html 页面的二维码。等价于密钥，泄露可重置。
_Avoid_: appToken+UID（全量 API，本项目不用）

ClawBot（微信龙虾通道）
: 微信官方插件（2026-03-22 推出，设置→插件→ClawBot 绑定）。WxPusher 经它把消息送进微信。限制约 10 条/24h（回复才重置）→ 只适合低频通知（本项目 ≤5 条/天）。
_Avoid_: WxPusher 客户端 App（消息的默认主通道，不是微信）

## 邮件推送（mailpush）

mailpush
: 设备端邮件推送工具（Rust，SMTP 直发 QQ 邮箱，5.5MB 全静态）。图片以 multipart/related + cid 内嵌正文直接显示（无需图床），凭证为 QQ 邮箱授权码。无状态幂等，去抖/冷却由调用方（visiond）负责。
_Avoid_: WxPusher（ClawBot 通道限 10 条/24h、App 图片需图床 URL）、pushplus（实名硬前置、图片服务收费、异步受理）

QQ 邮箱授权码
: 开启 QQ 邮箱 IMAP/SMTP 服务后生成的 16 位密码（替代登录密码做 SMTP AUTH）。等价于推送凭证，泄露可在邮箱设置里随时撤销。
_Avoid_: QQ 邮箱登录密码（禁止直接使用）

## 集成方式
独立交叉编译 + scp 部署
: visiond (C) 和 webd (Rust) 使用 buildroot 工具链独立编译，通过 scp 部署到板子的 /mnt/system/ 分区，不打包为 buildroot 包。
_Avoid_: buildroot 包集成

## 基础系统
buildroot (全量)
: 使用 Sophgo SDK 的 buildroot 构建系统编译 kernel/uboot/middleware/drivers，生成完整的 SD 卡启动镜像。

## Boot 更新（A/B 双槽）

boot.sd
: u-boot 通过 FIT 格式加载的内核镜像（kernel + fdt，SG2002 上不加载 initramfs）。OTA 更新的目标。
_Avoid_: uImage、Image（非 FIT 格式）

fip.bin
: FSBL 引导链（BL2 + OpenSBI + u-boot 打包，CVBL01 格式），由 fip_v2.mk 在 FSBL 构建时生成。不参与 A/B，基本不变，写坏需 SD 重刷。

boot_a.sd / boot_b.sd
: boot 分区内的 A/B 双槽内核镜像（去 ramdisk 后各 8.8MB）。u-boot 按 `boot_slot` 环境变量选择加载哪个槽。
_Avoid_: boot.sd（单槽时期的名字，A/B 后废弃）

boot_slot / bootcount / bootlimit / upgrade_available
: u-boot 环境变量（持久化在 /boot/uboot.env）。`boot_slot` 标记当前活动槽（a/b）；`bootcount` 连续启动失败计数；`bootlimit` 回滚阈值（=3）；`upgrade_available` 置 1 时才开始计数（Android A/B 门控，减少 FAT 磨损）。

uboot.env
: u-boot 持久化环境变量文件（FAT 分区 /boot/uboot.env，128KB），Linux 侧用 fw_setenv 读写。
_Avoid_: 内存态 env（CONFIG_ENV_IS_NOWHERE，重启即失）
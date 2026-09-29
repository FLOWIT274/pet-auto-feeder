# 0004: ewelink 控制端形态 = 独立 daemon + Unix socket

日期：2026-08-13
状态：已确认（用户拍板，设备端已就绪）

## 决策

ewelink-rs 作为**独立进程**部署在设备端（`/mnt/system/usr/bin/ewelink-rs`），
常驻 daemon 持有 mDNS 后台线程 + 设备热缓存，通过 Unix socket
（`$HOME/.ewelink-rs-daemon.sock`，一行指令一行 JSON）对外服务。
vision-server 的 webd 只做 **socket 客户端**（dispatch 封装），不库内嵌。
控制规则（如检测到猫触发插座）由视觉服务定制，本次不做 REST API。

## 为什么（权衡）

- **库内嵌 vs 独立进程**：库内嵌省一次 IPC，但 ewelink 栈（tokio + openssl +
  mDNS 线程 + 设备缓存生命周期）与 webd 进程耦合，一个崩全崩、升级只能整包换；
  独立 daemon 崩溃可自愈（CLI 薄壳自动拉起），缓存/登录态跨 webd 重启保留。
- **Unix socket vs REST/库调用**：socket 免认证免端口、本机进程天然隔离；
  后续若要局域网控制 UI 再加 HTTP 网关，不预先设计。
- 凭证独立存 `/etc/ewelink.env`（600），daemon 启动时注入，不写进 webd 配置。

## 代价

- 两次 IPC 拷贝（visiond → webd → daemon），控制频率低（秒级），可忽略。
- 需保证 S90ewelink 先于 webd 启动（依赖顺序：S30wifi → S90ewelink → webd）。

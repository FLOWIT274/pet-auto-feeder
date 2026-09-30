# LicheeRV Nano 项目知识

> 硬件：LicheeRV Nano（SG2002，C906B RISC-V 单核 + C906L RTOS 协核），256MB 物理 / ~128MB 可用（ION 75MB carveout）
> 详细交接文档：`docs/SESSION_SUMMARY.md`；ADR：`docs/adr/`

## 访问与网络

| 通道 | 地址 |
|---|---|
| WiFi SSH | `root@<BOARD_IP>`（DHCP 会漂移，漂移后扫 8080 端口或 mDNS licheervnano-XXXX.local） |
| USB RNDIS SSH | `root@10.86.142.1`（仅当板子 USB 为 device 模式时存在） |
| 管理台 | `http://<板IP>:8080` |
| 板子 AP 热点 | SSID `licheervnano-86141` 密码 `00000000`，网关 `10.141.86.1`（OTG host 模式下不可用） |

- 密码 root，sshpass；host key 变了用 `ssh-keygen -R <ip>`
- 安卓 APP（android-app/licheerv-admin.apk）：UDP 广播发现板子 → 通知直达管理页

## 当前架构（2026-08-21 定稿 v2）

```
摄像头(UVC, OTG host 模式) /dev/video0 MJPG 640x480 (~17fps, 25KB/帧)
  → vdec_stream_v4l2: DQBUF → libjpeg 软解(~70ms) → 居中裁剪 640x360 → NCHW 打包 → TPU 推理(31ms)
    (限帧 5fps; 检测框坐标 = 模型空间 640x360, y 需加裁剪偏移 60 映射回 480 高画面)
  → SHM /visiond_detect (2MB tmpfs): seq/ts/detections/原始MJPEG@264(len@260); JPEG 直通零编码
  → webd (:8080):
     WS /api/vision/ws = 检测数据(JSON 文本)
     GET /api/vision/frame = 原始 MJPEG 直接返回(带同帧缓存)
     with_shm 每次校验段 inode, visiond 重启自动重映射(防悬空 mmap 读死帧)
  → 浏览器: img 轮询画面 + WS 驱动画框(visDraw 按 naturalHeight 算裁剪偏移)
```

- **⚠️ 本板 DWC2 主机控制器无法建立高带宽等时传输**: 摄像头描述符最高提供 alt1=1024B×3事务/微帧
  (24.58MB/s), 但 YUYV 流激活时管道实际只落在 1024B×1 (8.19MB/s), YUYV 全分辨率实测 0 帧
  (800x600 也只有 0.5fps)。YUYV 免解码路线已否决; MJPG 是唯一可用采集格式
- VDEC 硬解只支持 H264/H265（PT_MJPEG 报 0xc005800c）；无 JPD 可用 → MJPG 用 libjpeg 软解
- **⚠️ VDEC 无 MJPEG 硬解（2026-08-24 三层实锤定案）**: ① API 层 PT_MJPEG=1002 存在且 CreateChn 竟返回 0;
  ② 内核 soph_vcodec.ko 仅含 h264c/h265c 时钟与格式, 设备节点只有 cvi_vc_dec0..8(9 个 H264/H265 核),
  全树无 JPD/JPEG 解码节点; ③ 实测 StartRecvStream(PT_MJPEG)=0xc005800c(NOT_SUPPORT), 对照组
  PT_H264 全流程通过。libvdec.a 里 CVI_ID_JPEGD 字符串是完整版 SDK 的残迹, 别再被误导
- **💡 但芯片本身有 JPU 硬件, 只是基线没带驱动 → 2026-08-24 已移植打通!**
  - 硅片证据: clk-cv181x.c 有 CV181X_CLK_JPEG/APB_JPEG; dts jpu 节点(IRQ20, reg 0x0B000000);
    freertos top_reg.h: JPU_BASE=0x0B000000
  - 驱动来源: sophgo/osdrv `sg200x-dev` 分支 interdrv/{vcodec,jpeg,cvi_vc_drv}
  - 板上产物(均在 /mnt/data/tpu/): cv181x_jpeg.ko(独立 JPU 字符设备 /dev/jpu, 补丁: 本地化 tWaitQueue
    等待队列) + vc_shim.ko(补旧 soph_vcodec 缺的 vcodec_lock/trylock/unlock/is_locked/
    vpu_set_common_memory 5 个导出) + cvi_vc_driver.ko(新版 VDEC/VENC 字符设备, 补丁: 解注释
    vdec/venc_vb_ctx 定义 + 无条件启用 CVI_VC_ENC_DEC_JPEG_TEST 钩子 + 编入 cvi_vc_getopt.o;
    modpost 桩表用板上 /proc/kallsyms 按模块生成)
  - **已转正(2026-08-24)**: 三个 .ko 部署到 /mnt/system/ko/ 并以 stock 命名
    (soph_jpeg.ko←cv181x_jpeg.ko, soph_vc_driver.ko←cvi_vc_driver.ko, 新增 vc_shim.ko),
    **开机加载入口是 /etc/init.d/S00kmod(内联 insmod 列表), 不是 loadsystemko.sh!**
    (loadsystemko.sh 是 OTA/手工用的, S00kmod 已补 vc_shim 行; stock 原件备份为 *.ko.stock)
  - 注意: /mnt/data/ota/stage/ 下有 OTA 暂存副本 loadsystemko.sh, 开机不执行, 别被误导
  - 实测: 内置钩子 `jpu_trigger /dev/cvi_vc_enc0 "cvi_jpg_test -t 1 -q -i x.jpg -o x.yuv"`
    解码 640x480 JPEG → 614400B packed-422(stride 1280), **27ms/次(含进程开销)**
  - 管线已集成: vdec_stream_v4l2 优先走 JPU(ioctl /dev/cvi_vc_enc0, /dev/shm 中转), 失败兜底 libjpeg
  - 注意: VDEC PT_MJPEG API 路径在新旧驱动上均报 BUF_FULL(原因未深究), 实用走测试钩子路径
- 板端固件自带 /usr/lib/libjpeg.so.9，交叉编译链接 buildroot output 的 per-package/libjpeg sysroot

## 组件清单

| 组件 | 源码 | 板端位置 | 说明 |
|---|---|---|---|
| webd | vision-server/webd | /mnt/system/usr/bin/webd + S90webd | 管理台 axum :8080，HTML 内嵌改页面必重编译 |
| ewelink-rs | ewelink/ | **/mnt/system/usr/bin/ewelink-rs** + S90ewelink | 插座控制 daemon，Unix socket，固件原生 pulse 全插位 |
| key-wipe.py | tpu-sdk 外单独 | /usr/bin + S99keywipe | 长按 User Key 5s 清空配网→AP 模式；60s 冷却 |
| wifiguard | S98wifiguard 内嵌 | /etc/init.d/S98wifiguard | WiFi 白名单守卫，10s 四重校验 |
| wifidisc.py | overlay usr/bin | /usr/bin + S97wifidisc | UDP 广播板子 IP（安卓 APP 发现用） |
| vdec_stream_v4l2 | tpu-sdk/vdec/src | /mnt/data/tpu/vdec_stream_v4l2 | MJPG 直采+libjpeg 软解+推理（V4L2 模式，手动拉起） |
| super_stream.sh | vision-server/ | host 运行 | 旧 host 推流守护（已退役备用） |

## 固件基线

- 打包机制：buildroot `BR2_ROOTFS_OVERLAY="board/sipeed/lichee_rv/overlay"` 自动合并
- overlay 内容清单见 `overlay/README-BASELINE.md`（init 脚本×6、二进制×3、配置、WiFi 白名单）
- ⚠️ etc/ewelink.env、boot/wifi.pass 含真实凭证，分发前脱敏
- 一键增量部署：`deploy/deploy_overlay.sh [IP]`
- 改 overlay 后三处同步：overlay 源 / buildroot output/target / 设备端

## 编译速查

```bash
# Rust (webd/ewelink-rs)：export CC/AR_riscv64gc_unknown_linux_musl=.../riscv64-unknown-linux-musl-{gcc,ar}
cargo test --release && cargo build --release --target riscv64gc-unknown-linux-musl
# ewelink-rs 特有：rust-lld + crt-static（详见 git 历史，~/.cargo/config.toml 已配）

# vdec_stream_v4l2 (C, V4L2+JPU+TPU)：直接跑脚本，可逐字节复现已部署二进制
tpu-sdk/vdec/build.sh                 # → bin/vdec_stream_v4l2（-O3 -funroll-loops -march=rv64gcv0p7）
# 等价手工命令（RVV 必须开，否则 jpu_nv12_to_nchw_rvv 走不了向量化）：
LJ=LicheeRV-Nano-Build/buildroot/output/per-package/libjpeg/host/riscv64-buildroot-linux-musl/sysroot/usr
$HOME/toolchains/riscv64-linux-musl-x86_64/bin/riscv64-unknown-linux-musl-g++ -O3 -funroll-loops -march=rv64gcv0p7 \
  -o out tpu-sdk/vdec/src/vdec_stream.c \
  -I tpu-sdk/tpu-sdk/include -I LicheeRV-Nano-Build/middleware/v2/include -I $LJ/include \
  -I LicheeRV-Nano-Build/linux_5.10/build/sg2002_licheervnano_sd/riscv/usr/include \
  -Wl,--start-group LicheeRV-Nano-Build/middleware/v2/lib/{libvdec.a,libsys.a} tpu-sdk/tpu-sdk/lib/*-static.a -Wl,--end-group \
  -L$LJ/lib -ljpeg -latomic -lpthread -lm

# 首帧模型输入 dump（调试用，默认关）：VDEC_DUMP_FRAME=1 → /tmp/frame0.rgb（tmpfs，零 SD 写入）
#   （历史上曾无条件写 /mnt/data/frame0.rgb，是排查 YUYV/JPU 布局时的临时手段，2026-09-09 改掉）

# Android APK：android-app/build_apk.sh（SDK 在 ~/android-sdk，手工 aapt2/d8/apksigner 无 gradle）
```

## 部署

- **路径铁律**：`S90webd`/`S90ewelink` 调的是 `/mnt/system/usr/bin/{webd,ewelink-rs}`（`/mnt/system` 是 rootfs 上的**普通目录**，不是独立分区）。
  overlay 里必须放 `mnt/system/usr/bin/`，放 `usr/bin/` 服务起不来（2026-09-09 实测踩过：deploy_overlay.sh 长期只写 `/usr/bin/webd`，
  而服务跑的是 `/mnt/system/usr/bin/webd`，两边各留一份互不相干）。
- webd：S90webd stop → scp → start（**先 killall 防 Text file busy**）；`deploy/deploy_overlay.sh` 已改为按 md5 跳过未变文件（不再反复写 20MB）。
- HTML 是 include_str! 内嵌 → 改前端必须重编译
- SHM 布局改动需同步 vdec_stream.c 和 webd vision.rs 两侧

## 关键机制

### WiFi 白名单
- S30wifi（已改造）：只连 /boot/wifi.ssid 指定网络；忽略整体 wpa_supplicant.conf；生成配置强制 update_config=0 + 单 network
- 无凭证 → AP 配置模式（热点 + 80 端口 wificfgd）
- S98wifiguard 每 10s 校验：ssid 白名单/network≤1/无任意连接块/禁 update_config=1，异常重建+重启 WiFi

### 自动喂食控制
- 触发：狗连续 ≥1.2s 且窗口内无猫且均值 >65%（窗口 1.2s 与阈值 65% 为**代码内固定常量** `control.rs::DOG_WINDOW_S/DOG_MIN_SCORE`，页面不可调、也无环境变量；`WEB_CTRL_DOG_SECONDS/WEB_CTRL_DOG_MIN_SCORE` 已废弃）
- 配额默认 1 次/日（6 点刷新）、通电时长可配；SHM 轮询 100ms；持久化 /mnt/data/webd_control.json
- 动作：抓帧 → mailpush 邮件带照片 → ewelink pulse 全插位
- ewelink 云控稳态 ~1-2s/次（免费账号每次触发走 OAuth 有配额）

### eWelink 要点
- daemon 唯一控制点，token 落盘共享（最新登录踢旧的，401 自动重登）
- 固件 uiid=138 虚报 4 通道实际 1 插座 → 全插位操作
- 固件原生 pulse 恒定 +200~300ms 偏移

### 安卓 APP 发现协议
- UDP 37777：板子每 3s 广播 JSON + 应答 "LICHEERV-DISCOVER?" 探测
- APP 监听+主动探测 → 通知点击直达浏览器管理页

## 活跃坑位（必读）

1. **struct input_event = 24 字节**（64 位 timeval 16B + 8B）；read <24 必 EINVAL。FIFO 注入测试不校验大小会掩盖此 bug2. **busybox pkill -f 对 python 进程无效**且模式含明文会匹配当前 shell 自杀 → pidfile kill + ps 扫描 kill -9；命令行含关键字时用 [x]xx 字符类
3. **v4l2_format 64 位布局**：union 8 字节对齐，pix 从 +8 开始（type@0）。跨平台结构必须 dump 驱动回填验证；ENUM_FRAMESIZES=56B（本板 uvcvideo 对其返回 ENOTTY，探测分辨率只能用 S_FMT 试探）
4. **QBUF 后 mmap 归驱动所有**，先拷贝再归还，否则读到撕裂帧
5. **JS const TDZ**：顶层声明顺序错误中断整个 script；window.x 访问 let 变量永远 undefined
6. **createObjectURL 必须 revoke**，10fps 流下泄漏即 GC 风暴
7. musl-g++ 链接 C++ 静态库需处理 __cxa_pure_virtual（g++ 驱动）/__atomic_compare_exchange_1(-latomic)；mmap 返回要 cast；**静态库有循环依赖时用 -Wl,--start-group/--end-group**
8. Android：adb 启动未导出服务被拒（从 Activity 拉）；MulticastLock 必须持有才能收广播；targetSdk<33 免运行时通知权限
9. loadavg 在 USB IO 等待时会虚高，判断 CPU 用 top 的 idle%
10. 板上无 pgrep/timeout/v4l2-ctl/ffmpeg；ps 是 busybox 版；无限流测试用 `(wget -O f -T N &)` 看文件大小
11. **SHM 布局**：头部 0..264 已塞满（seq@0/ts@8/num@16/dets@20/jlen@260），格式魔数只能放尾部（+12 与 ts u64@8..16 重叠会互踩）；当前 JPEG 模式不写魔数（0=JPEG），YUYV 魔数约定在 SHM_SIZE-4
12. **webd 常驻 mmap 必须校验段 inode**：visiond 重启 unlink+新建段后旧映射变死数据（曾致画面/检测冻结）；with_shm 每次 stat /dev/shm/<name>（注意 shm_open 名字≠文件路径）比对 inode 自动重映射
13. **V4L2 主循环必须有显式 g_stop 检查点**：帧流不停时循环永不阻塞，信号标志无人读（曾致 SIGTERM 不退出）
14. **该 uvcvideo 对不支持的 S_FMT 静默回退不报错**（曾把 YUYV 请求静默变成 MJPG 默认档）→ S_FMT 后必须 G_FMT 回读校验
15. **DWC2 高带宽等时缺陷**（见架构节）：YUYV 类未压缩格式全分辨率 0 帧，别再尝试；MJPG/低带宽等时正常
16. **陈旧 socket 会伪装成"服务在跑"**：`S90ewelink start` 原只查 `[ -S $SOCKET]`，daemon 被 -9/断电后会留下 socket 文件 → 之后每次开机都跳过启动。
    2026-09-09 实测该 bug 让 ewelink daemon 自 08-21 起一直是死的（喂食链路整条失效，管理台却显示正常）。
    已修为**进程+socket 双校验**：socket 存在但 `ps` 无 `[e]welink-rs daemon` → 删 socket 再拉起。
17. **二进制放错目录 = 服务静默不启动**：见"部署"节——webd/ewelink-rs 必须在 `/mnt/system/usr/bin/`。
18. **/var/log → /tmp（tmpfs）**：重启后 dmesg/webd/vdec 日志全丢。2026-09-09 一次疑似死机（板子 09:41 起无响应 7 分钟，硬复位后 FAT /boot 报 "not properly unmounted"）因此**无法定因**。
    排查线索：20MB 单次 dd 写 SD 不会挂（已复现两次正常）；视觉热路径无 SD 写入（`/proc/<pid>/fd` 全指向设备/tmpfs）；2GB swapfile 与 rootfs 同在 SD 卡；bluetoothd 在跑且 BT 固件 `uart_flowctrl:1`。要定位必须先把内核日志落到 SD（待办）。

## 已否决方案（防重蹈）

- WebView 内嵌管理台：性能无法达标（JS 搬运视频帧天生卡 + LAYER_TYPE_HARDWARE 破坏合成 + canvas dpr 灾难），用户定版极简跳转
- STA+AP 并发：性能顾虑被用户否决
- 分类器路线（ADR 0003）：cv181x INT8 全链路失败 + 结构性缺陷，用检测器
- yolov8m：CV181x 官方无模型，自转性价比不如 yolov8s
- WxPusher 微信通道：ClawBot 限 10 条/24h；邮件内嵌图片胜出（mailpush 定案）
- **YUYV 直采免解码**（2026-08-21 否决）：DWC2 高带宽等时缺陷致全分辨率 0 帧，"CPU ~8%" 的验证结论是 S_FMT 静默回退 MJPG 造成的假象；MJPG+libjpeg 软解替代（idle 53%）

## 待办

- [ ] 手机浏览器人工确认画面+画框（服务端全链路已验证；画框需镜头前有猫/狗/人实测）
- [ ] 固件分发前脱敏 ewelink.env / wifi.pass
- [x] vdec_stream_v4l2 开机自启已由 S96vision 固化（overlay），手动 nohup 仅限开发调试

## JPU 硬解路径 + 卡死根因（2026-08-24）
- 用户移植三个内核模块：cv181x_jpeg.ko（soph_jpeg 移植）/ cvi_vc_driver.ko（soph_vc_driver，MaxVencChnNum=9 MaxVdecChnNum=9）/ vc_shim.ko（新增）；S00kmod 内联 insmod 开机加载；/dev/cvi_vc_dec0-8 + enc0-8。
- JPU 硬解链路（vdec_stream_v4l2 V4L2 模式定稿 2026-08-25）：MJPEG 640x480 采集 → jpu_decode_to_nchw（ioctl CVI_VC_ENC_DEC_JPEG_TEST 传 `cvi_jpg_test -t 1 -q -ci 1`，驱动内核态解码 → /dev/shm/fr.yuv0.yuv 614400B=NV16）→ jpu_nv16_to_nchw → TPU；libjpeg 软解兜底（板端 libjpeg.so.9，编译 -I buildroot sysroot + -ljpeg）。
- **旧 packed-422 转换（jpu_pack422_to_nchw）已废弃**：`-t 1` 默认输出 planar I422，`-ci 1` 输出 NV16；`-t 0` 是非法模式（曾致 ioctl 失败静默回退 libjpeg）。详见下方"测试图案真相"。
- **YUYV 索引坑（历史）**：旧 YUYV 每像素 2 字节，转换必须 `row + xx*2`，U/V 每 2 像素共享一组 `(xx>>1)*4`；写成 `xx*4` 会读穿行尾+色度错位。jpu_pack422_to_nchw 与旧 yuyv_to_nchw 都有此 bug（已修）。查表 g_vt/g_ut 内部已 -128，调用处不能再减（双重 -128 也修过）。NV16 转换已按新布局实现，不再有该问题。
- **卡死根因（部署后随机失联的谜底）**：host 侧 super_stream 守护 ensure_board 用 `pidof vdec_stream` 检测，匹配不到新进程名 vdec_stream_v4l2 → 一旦 DHCP 把旧 IP 分回，它就拉起 FIFO 版 vdec_stream（占 VDEC/VPU 通道）→ 与 JPU 直采进程抢 VPU → 卡死。**解法：停 host 守护 + S96vision 开机自启（唯一视觉进程）**。
- S96vision：等 /dev/video0 就绪 → LD_LIBRARY_PATH 拉起 vdec_stream_v4l2（5fps 限帧）→ kill -0 健康探测（busybox ps 无 -p）。已固化 overlay。
- 解码性能：dec+scl=49.5ms（JPU 硬解 422 打包转换占大头）fwd=29ms；5fps 限帧下稳定运行 3min+ 无卡死。
- 10fps 提速：限帧 200→100ms。实测 9.6fps 稳定（dec+scl=51.4ms + fwd=29.1ms = 80.5ms/帧，理论上限 12.4fps）。系统负载含 USB IO 虚高但 CPU 未饱和。

## JPU 硬解"测试图案"真相（2026-08-25 更正，检测框失踪的真正根因）
- **`-t 1` 才是解码模式**（源码 `CVIJPGCOD_DEC = 1`，`CVIJPGCOD_ENC = 2`）；`-t 0` 非法（驱动报 `unrecognized mode type parameters`，ioctl 失败 → 静默回退 libjpeg 软解）。
- **默认输出是 planar I422**（Y 640x480 + Cb/Cr 各 320x480 = 614400B），不是 packed-422。
  - 之前把 planar I422 当成 packed-422（stride 1280 YUYV）读取，导致"行 240 均值突变/下半部灰/检测框失踪"——这是**布局误读**，不是测试图案模式。
  - `-ci 1` 可让 Cb/Cr 交织成 **NV16**（Y 平面 640x480 + CbCr 交织 640x480 = 614400B），转换更紧凑。
- **正确命令**：`cvi_jpg_test -t 1 -q -ci 1 -i in.jpg -o out.yuv`；vdec_stream.c 已按 NV16 布局实现 `jpu_nv16_to_nchw`（居中裁剪 60..419 行，不做 2:1 降采样）。
- **板上曾用旧 JPU 驱动**：`/mnt/system/ko/soph_jpeg.ko` 是 30656B 旧版，`cvi_jpg_test -t 1` 在内核 `CVIJpgOpen` oops/挂死；必须用本地移植版 `cv181x_jpeg.ko`（31072B，md5 3cec8d6f…）。已同步到 `/mnt/system/ko/soph_jpeg.ko` 和 `/mnt/data/tpu/cv181x_jpeg.ko`。
- **性能（-O3，2026-08-25 实测）**：ioctl=27ms + io=1ms + conv=15-16ms，dec+scl≈45-48ms，fwd≈29ms，10fps 达成（此前 -t 0 软解回退 dec+scl≈75ms、9.3fps）。
- **decode_stride 隐藏 CPU 大头（2026-08-25 修）**：原来对全部 5040 个网格单元每帧算 3 次 `expf` sigmoid（约 1.5 万次软浮点 expf/帧）。sigmoid 单调 → 改成先找最大 logits，只对最优类算一次 expf；`[cls]` 统计同理。**注意：`boxes[].score` 必须赋 sigmoid 值，不能赋原始 logit（否则出现 >1 的分数）**。
- **稳定 10fps 的关键是精确限帧（2026-08-25 修）**：摄像头裸测 17.7fps（帧间隔 ~48-52ms），不是摄像头瓶颈。原限帧用 `usleep(10000)` 10ms 步进：处理一帧 ~77ms 后要补 23ms，10ms 粒度会多睡到 30ms → 周期 ~107ms → 9.4fps。改为 `usleep(剩余 ms)` 精确补足到 100ms 后稳定 10.0fps（600 帧实测）。
- **摄像头 USB 掉线自愈（2026-08-25 加）**：板上 UVC 摄像头会间歇性 disconnect/reconnect（疑似供电/线材/DWC2），掉线时 `/dev/video0` 消失、重连后 minor 可能漂移（video1/video2）。已加自愈：
  - `vdec_stream`：DQBUF 持续 `ENODEV/EIO >2s` → 清理 SHM 后 `_exit(3)`；打开指定设备失败时自动扫描 `/dev/video0..3` 挑能协商 MJPG 640x480 的节点。
  - `S96vision`：常驻 watchdog（每 5s，PID `/var/run/vdec_watch.pid`）——有任意 `/dev/video[0-3]` 且视觉进程已死 → 自动重启。`stop` 会同时杀 watchdog。
  - **DWC2 控制器规避（2026-08-25 加）**：摄像头缺失 ≥60s → watchdog 软复位 DWC2（`echo 4340000.usb > .../dwc2/unbind` + `bind`，免整机重启）；软复位后仍缺失 ≥360s → `reboot` 兜底。日志 `/tmp/vdec_watch.log`。
  - **vdec 卡死心跳（2026-08-25 加）**：watchdog 读 `/dev/shm/visiond_detect` 的 sequence（vdec 每帧递增）——进程活着但 sequence 冻结 ≥20s（或 SHM 缺失 ≥30s）→ 先 `kill -9` vdec、再 DWC2 unbind/bind 复位 + 重启。解决"进程活着但 DQBUF/JPU/TPU 卡死"检测不到的问题。
  - **webd 保活（2026-08-25 加）**：S96vision 每 5s 同时查 webd PID + `http://127.0.0.1:8080/api/health`，不可用自动重启；S90webd 手动 stop 会写 `/var/run/webd.managed-off`，watchdog 不与之抢。
  - **webd 陈旧帧标记（2026-08-25 加）**：SHM `timestamp_ms` 是 CLOCK_MONOTONIC，webd 用同源时钟算 age，>5s 无新帧 → `/api/vision/meta` 返回 `stale:true`，前端徽标显示"在线（画面停滞）"并停止拉旧帧。
  - **注意**：不要在摄像头正在推流时手动 unbind/bind DWC2，会把 uvc 流卡在 busy/无帧状态（实测踩过），需要整机重启清理；watchdog 复位前会先等节点消失（掉线路径）或先 `kill -9` vdec 关掉 V4L2 流（心跳冻结路径），确保没有进程在推流。

## cvi_vc_driver 驱动补丁（2026-08-25 编译根治）
- **问题 1**：`cvi_jpg_test` 每帧用 `pr_info` 打印 `type/readLen/iDataLen` → dmesg 刷屏（10fps=30 行/s）。
- **问题 2（Oops）**：`write_yuv_file()` 在解码失败/坏帧时 `vbY.virt_addr == NULL`，`kernel_write→memcpy` 空指针 → 内核 Oops（cause=0xd, badaddr=0x0）。
- **补丁**（`cvi_jpg_codec.c`）：注释掉三处解码路径 `pr_info`；`write_yuv_file()` 开头加 `addrY_v/addrCb_v/addrCr_v == NULL` 校验，NULL 直接跳过写文件。
- **重新编译方法**（本环境实测可行）：
  1. 取 sophgo/osdrv `sg200x-dev` 分支源码（`/tmp/sophgo-osdrv`）；
  2. 打上本地 jpu_port 补丁：`cvi_vc_drv_patched.c→cvi_vc_drv.c`、`cvi_venc_kernel_patched.c→module/src/cvi_venc.c`、`cvi_vdec_kernel_patched.c→module/src/cvi_vdec.c`、`cvi_vc_drv_Makefile.patched→Makefile`；
  3. 修 `main_enc_cvitest.c` 的 `roi_cfg_type` 版本不匹配（注释该行）；
  4. 依赖符号用本地 v2 树 `osdrv/interdrv/v2/{base,sys,vcodec}/Module.symvers` + 从 `vc_shim.ko`/`cv181x_jpeg.ko` 提取导出符号，合成 `KBUILD_EXTRA_SYMBOLS`；
  5. `make ARCH=riscv CROSS_COMPILE=... CVIARCH=CV181X CVIARCH_L=cv181x LDDINCDIR=<kernel>/riscv/usr/include KBUILD_EXTRA_SYMBOLS=<symvers> -C <kernel_build> M=<cvi_vc_drv> modules`。
- **部署**：新 `cvi_vc_driver.ko`（md5 1b8bb7c7…）已替换 `/mnt/system/ko/soph_vc_driver.ko`、`tpu-sdk/vdec/jpu_port/bin/cvi_vc_driver.ko`、SDK overlay `buildroot/board/cvitek/SG200X/overlay/mnt/system/ko/soph_vc_driver.ko`。
- **验证**：重启后 dmesg 中 `type/readLen/iDataLen` 计数 = 0；模块含 `write_yuv_file: frame buffer virt_addr NULL, skip` 防护。

## 硬解+推理链路优化（2026-08-25，②③⑤）
- **② IO 优化**：输入 JPEG 用常驻 fd + `ftruncate`+`pwrite`；输出 YUV 用常驻 fd + 常驻 `mmap`（只映射一次）。`io` 从 1ms → 0ms。
- **③ NV12 直出（驱动）**：`cvi_jpg_codec.c` 的 `write_yuv_file` 交织 UV 只写偶数行 → 输出从 NV16 614KB 变 NV12 460KB（-25%）。`ioctl` 27→20ms。
- **⑤ RVV 向量化（conv）**：`jpu_nv12_to_nchw_rvv` 用 `riscv_vector.h`（`-march=rv64gcv0p7`），每行标量展开 U/V 后按 16 像素向量算 RGB（定点整数：`(y<<10)+色差 >>10 → clamp → >>1`）。`conv` 16→8ms。
- **最终性能**：`dec+scl≈30ms`（ioctl 20 + io 0 + conv 8），`fwd≈29ms`，**10.0fps 稳定**。
- **坑**：RVV 里必须先 `>>10` 再 clamp 再 `>>1`，顺序反了会全 0（曾致 0 检出）；UVC 帧尾有 padding，不要用严格 EOI 校验（会误杀一半帧）。
- **编译**：vdec_stream 用 `-march=rv64gcv0p7 -O3`；驱动重建方法见上文。
  - **坑**：判断摄像头存在要用 `ls /dev/video[0-3]`，不能列全部四个路径（缺一个就返回非零）。
  - **坑**：watchdog 后台 subshell 必须 `>/dev/null 2>&1` 重定向，否则 SSH 断开后 `start_daemon` 里的 `echo` 触发 SIGPIPE 把 watchdog 打死。
  - 手动 kill 测试：kill -9 后 8s 内自动恢复 10fps。
- **排查方法论（有效）**：先确认 JPU ioctl 是否真的成功（`/dev/shm/fr.yuv0.yuv` 的 mtime 是否每帧更新）→ 用 `jpu_trigger` 直接调钩子 → 拷贝输出到 host 按 planar I422 / NV16 布局做相关性验证，而不是凭"测试图案"臆断。

## dmesg 噪声清理（2026-08-25）
- **AICWFDBG WiFi 刷屏**：`aicwf_dbg_level` 默认 `LOGERROR|LOGINFO|LOGDEBUG|LOGTRACE|LOGFW`（1039）。已在 `S25wifimod` 的 `insmod 3rd/aic8800_fdrv.ko aicwf_dbg_level=1` 固化；`aic8800_bsp` 的 `aicwf_dbg_level_bsp` 同样默认改为 `LOGERROR`（源码 `aic_bsp_main.c`）。
- **get_txpwr_max 无条件 printk**：`rwnx_platform.c` 中每次取功率都打印，已注释（需重编 `aic8800_fdrv.ko`）。
- **BT 调试刷屏**：`aic_btsdio.h` 的 `AICBT_DBG_FLAG=0` 且 `AICBT_INFO` 改空宏（保留 WARN/ERR），消除 `aic_btsdio: hci0,...` 每次收发包打印。
- **VPSS open/release 刷屏**：`vpss_core.c` 的 `pr_info("vpss_open"/"vpss_release"/"open %d times"/"close %d times")` 已注释（重编 `soph_vpss.ko`）。
- **VC 驱动 channel 刷屏**：`cvi_vc_drv.c` 的 `open/close channel no.` 已注释（重编 `cvi_vc_driver.ko`）。
- **RTC 周期刷屏**：`rtc.c` 的 `dev_notice("time set/read as ...")` 改为 `dev_vdbg`（重编 `soph_rtc.ko`），保留开机注册/校时那几条正常日志。
- **验证**：重启后 dmesg 约 364 行，`AICWFDBG/get_txpwr_max/vpss_open/channel/cvi_rtc time/aic_btsdio: hci0` 计数均为 0；WiFi/视觉/10fps 正常。

## 多整点配额刷新（2026-08-25 新增）
- **模型**：`refresh_hour: u32` 升级为 `refresh_hours: Vec<u32>`（0..=23，排序去重，至少一个）。持久化同时写 `refresh_hour`（首元素，兼容旧版）和 `refresh_hours`。
- **刷新语义**：`quota_slot_index = day * len(hours) + 当天已过整点数`，索引变化即到点发放新配额；支持一天多个整点（如 06:00/18:00）。
- **切换对齐**：`update_config` 修改刷新时刻后立即用新时刻列表重算 `last_quota_day`，避免切换时产生一次多余重置。
- **UI**：设置页“参数配置”改为 24 个整点 chip 多选（至少保留一个），保存提交 `refresh_hours`。
- **验证**：API 保存 `[18,6]` → 归一 `[6,18]`，`next_refresh="每日 06:00/18:00"`，状态文件落盘；恢复 `[6]` 后槽位正确回 20691。

## 配额用尽暂停推理链（2026-08-25 新增）
- **机制**：webd `control.rs` 在 `used_today >= quota` 时写 `/tmp/vdec_pause`；`vdec_stream.c` V4L2 主循环检测到该文件后暂停推理。
- **两种模式**（设置页开关 `keep_video_on_pause`，默认关）：
  - **关闭（默认，全停）**：只写 `/tmp/vdec_pause`，vdec 检测到后清理 SHM 并 `_exit(0)` → 摄像头/实时视频一起关，最省资源；S96vision watchdog 看到 pause 文件后**不拉起/不心跳/不复位**，配额刷新时 webd 删除 pause 文件，watchdog 5s 内自动重启 vdec。
  - **开启（保留视频流）**：同时写 `/tmp/vdec_keep_video`，vdec 不退出，只跳过 JPU 解码 + RVV 转换 + TPU 推理，继续采集并发布实时 MJPEG（空检测）→ 实时画面和 SHM 心跳保持。
- **恢复**：配额刷新（`refresh_if_due`）、配额清零（`reset_quota`）、修改配额/时刻（`update_config`）都会 `sync_pause()` 清除/重建标志。
- **调试模式强制恢复**：开启 webd“调试模式（dry_run）”时 `set_force_video(true)` 清除 pause/keep_video 标志 → 配额用尽也能强制拉起推理+视频流；关闭时 `set_force_video(false)` 按配额状态恢复暂停。`inference_paused` 在调试强制下返回 false。
- **验证**：全停模式 → vdec 退出、SHM 删除、watchdog 不拉起；配额清零后 watchdog 自动重启。保留模式 → 推理日志停、SHM sequence 仍 10fps 递增；调试模式开启 → 全停状态下 vdec 自动拉起且推理/视频恢复，关闭后回到暂停；`inference_paused`/`keep_video_on_pause` 状态正确。

## 崩溃现场日志（2026-09-09 新增，解决"死机后无日志可查"）
- **问题**：`/var/log` → `/tmp`（tmpfs），重启即丢；2026-09-09 一次疑似死机因此无法定因。
- **方案**：`S97logpersist` → `/usr/bin/crashlog.sh`，把易失日志镜像到 SD：
  - `/mnt/data/log/messages.log`：`tail -f /var/log/messages`（klogd 内核消息 + syslog 用户消息）；每次开机补一次开机段。
  - `/mnt/data/log/status.log`：每 60s 一行 `时间 up= 内存 used/avail swap load wlan0/link/信号 视觉序号`；**视觉序号连续冻结 ≥3 个采样会附 `/tmp/vdec_v4l2.log` 尾部**（卡死现场）。
  - 两文件各 **1MB 封顶**（超限保留最后 256KB，**原地截断**保持 inode，否则 tail 的 fd 指向旧 inode 会丢后续日志）。
- **上次是否正常关机**（启动横幅）：`stop` 写 `.last-stop`——`rcK` 关机→`shutdown`；手动→`manual`；覆盖运行中实例→`restart`；**文件不存在 = 断电/死机 → `⚠ 上次未正常关机`**，并用 dmesg 佐证（`not properly unmounted` / ext4 `recovery complete`）。注意 `start()` 里不能调 `stop()`（会写假标记）。
- **用法**：`/etc/init.d/S97logpersist start|stop|status`；死机复位后先看 `/mnt/data/log/messages.log` 的横幅与尾部、再看 `status.log` 的内存/swap 走势。
- 写入量：状态行 ~150B/分钟，messages 仅在有新 syslog 行时追加（dmesg 噪声已清理）。

## 配额刷新蜂鸣提醒（2026-08-25 新增；2026-09-30 修正为高电平触发）
- **硬件**：**高电平触发**的蜂鸣器**模块**（自带驱动电路），GPIO 仅作触发信号、不驱动负载 → 无拉电流问题。
  **静音(idle) = 低电平；鸣叫(active) = 高电平**。
  ⚠️ 早期文档误记为"低电平触发"，导致脚本极性写反（平时高、响时间歇拉低）——已修正。
- **引脚（源码级核对）**：**A19 = GPIOA19 = pad `JTAG_CPU_TMS` = Linux GPIO 499**（bank A 基址 480 = `ARCH_NR_GPIOS` 512 − 32，+19）。
  - FMUX 寄存器 **`0x03001064`**（3bit 字段 offset 0 / mask 0x7；**该寄存器仅此脚使用**，可整字写入）；
    功能值：`0=JTAG_CPU_TMS 1=CAM_MCLK0 2=PWM_7 3=XGPIOA_19 4=UART1_RTS 5=AUX0 6=UART1_TX 7=VO_D_28`
    （见 `freertos/cvitek/hal/cv181x/config/cv181x_pinlist_swconfig.h` + `cv181x_reg_fmux_gpio.h`）。
  - 原厂 u-boot 把它置为 `0x4`=UART1_RTS（`cvi_board_init.c:103`）。**对高触发蜂鸣器这是危险的**：
    UART MCR 复位值=0 → RTS 去断言 → 16550 的 RTS# 为高 → pad 高 → **开机即持续长响**。
- **开机处理（u-boot 层，补丁 01）**：u-boot 直接把该脚设为 `XGPIOA_19`(0x3) 输出低（静音），
  不再用 UART1_RTS。本项目不用 UART1（BT 走 **SDIO**：`aic_btsdio.c`；inittab 无 ttyS1 getty），
  所以放弃该 pad 无代价。这样从最早的可控点就保证静音，不依赖任何用户态脚本。
  - 另有 `S20buzzer`（`/etc/init.d/`）在启动早期调 `buzzer_beep.sh 0` 再确认一次 GPIO 低——
    防止 OTA/换固件等场景下 u-boot 改动未生效时长响。
- **Linux sysfs 陷阱**：Linux 5.10 的 `gpiolib-sysfs.c` 中 **`"out"` 等价于 `"low"`**
  （`else if (streq(buf,"out") || streq(buf,"low")) direction_output_raw(desc,0)`）。
  所以写 `echo out > direction` 会**先把脚拉低**；对高触发模块这无害（本来就是静音电平），
  但对低触发模块会造成"多响一下"的毛刺。脚本统一用 `echo low > direction` 显式表达意图。
- **脚本**：`/usr/bin/buzzer_beep.sh [次数]`（overlay 固化，**755**）——
  pinmux 写 `0x03001064=0x3` → export 499 → `direction=low`（静音）→
  循环：拉高 0.15s（响）→ 回低 → 间隔 0.25s。2 下 = 0.55s（实测 0.561s）。
  - **节奏可配置**（2026-09-30 新增）：读 `/etc/buzzer.conf` ——
    `BUZZ_COUNT`（几下）/ `BUZZ_ON`（每下时长）/ `BUZZ_GAP`（间隔）/ `ACTIVE_HIGH`（极性）/ `RESTORE_MUX`。
    改完**立即生效、无需重启**。优先级：**命令行次数 > 显式环境变量 > 配置文件 > 内置默认**。
    webd 调用时**不传次数**（此前硬传 `"2"`，会盖掉配置），因此配置对它同样有效。
    试听：`/usr/bin/buzzer_beep.sh`（按配置响）或 `/usr/bin/buzzer_beep.sh 5`（临时 5 下）。
    真机实测：把配置改成 3 下/0.35s/间隔 0.30s，未重启任何服务即生效
    （寄存器实测脉冲 0.354/0.345/0.356s，间隔 ≈0.30s）。
  - **`buzzer_beep.sh 0`** = 只切 pinmux + 输出低，完全不响（S20buzzer 用的就是这个）。
  - **`RESTORE_MUX` 默认 0**：响完**保持 GPIO 输出低**，**不**还原成 UART1_RTS
    （还原会让高触发模块长响）。代价是该 pad 不再作 JTAG TMS / UART1_RTS。
  - `devmem` 在 `/usr/sbin`，init 脚本 PATH 未必含它 → 脚本用绝对路径回退。
  - `trap INT TERM HUP` → 先回静音低电平（webd 5s 超时会 kill 它），实测中断后末态为低。
- **集成**：webd `control.rs` 的 `refresh_if_due()` 返回 `bool`；`run()` 轮询（100ms）检测到配额槽变化时
  `tokio::spawn` 调 `buzzer_beep.sh 2`（异步不阻塞，5s 超时）；**无 HTTP/UI 手动触发入口**，手动测试直接 SSH 跑脚本。
- **验证**：桩测试确认调用序列为 `0x3 → 低 → 高 → 低 → 高 → 低`（正好 2 个高脉冲），
  中断路径末态为低，计时 2 下=0.561s / 3 下=0.965s。
  真机寄存器验证：`buzzer_beep.sh 2` 退出 0，`DATA` bit19 正确产生 高→低 脉冲并回落静音。
- **听感确认（2026-09-30，接上模块后）**：**低电平 = 安静**（保持 20s 无声）；
  高电平 = 响（`buzzer_beep.sh 3` 的 3 声短响 + 2 秒长音都听到）。**极性判定闭环，"高电平触发"成立** ✓
- **部署**：已加入 `deploy/deploy.sh` 拷贝清单 + `chmod 755`。

### 上电静音的根治（u-boot 层，2026-09-30 真机刷入并验证）
- **为什么用户态不够**：实测该 pad **复位默认有内部弱上拉 → 高**；原厂 u-boot 又把它配成
  `UART1_RTS`，UART MCR 复位值=0（RTS 去断言）→ 引脚**高**。所以从**上电**到 `S20buzzer`
  执行（实测 `uptime≈4.85s`）之间高触发蜂鸣器会持续响；用户态最早只能到 rcS，覆盖不了这段。
- **"在 u-boot 里写 UART1 MCR 让 UART 自己拉低"是死路**：实测写 `0x04150010=0x2` 后 pad 确实变低，
  但 **内核 probe 8250 时会把 MCR 写回 0**（`unbind`/`bind` 可复现）→ 只能维持几秒。
- **有效做法**：u-boot 直接把 A19 改成 `XGPIOA_19` 输出低（先写电平位、再设方向，避免毛刺），
  见 `patches/01-board-bringup.patch` 的 `cvi_board_init.c`。
- **刷写位置**：u-boot 在 `fip.bin` 里（魔数 `CVBL01`），而 `fip.bin` **是 FAT 引导分区中的文件**
  （实测文件内容起点 = 裸卡扇区 169；p1 起始扇区=1，BootROM 走 FAT 读取）。
  **`fip.bin` 没有 A/B 兜底**（`boot_a.sd`/`boot_b.sd` 只是内核 FIT），官方 OTA 包也不含它 →
  刷坏必须取 SD 卡恢复。刷法：`dd if=新fip.bin of=/boot/fip.bin bs=4096 conv=notrunc`（保持簇链），
  再 `sync` + 重启；用 `dd` 而非 `cp` 可避免 O_TRUNC 触发簇重分配。
- **可复现的验证方法**：把 `buzzer_beep.sh` 整个移走并停用 `S20buzzer`，使**任何用户态都无法改 A19**，
  重启后读寄存器 —— 得 `FMUX=0x3`、`DATA` bit19=0、`DIR` bit19=1，即证明由 u-boot 独立完成。
  同法在刷机前测得 `FMUX=0x4`。
  ⚠️ 只停 `S20buzzer` 不够：**webd 开机也会调蜂鸣脚本**，会把 pinmux 改成 0x3 而冒领功劳。
- **残留窗口（实测）**：上电到 u-boot 执行 `board_init` 之间，pad 仍是复位默认（弱上拉=高）。
  刷入新 u-boot 后真机听感：**开机只剩开头约 1 秒的一声**，之后全程安静
  （修复前是持续响到 `uptime≈4.85s`）。要连这 1 秒也消掉，需在
  **A19 与 GND 间加 10kΩ 下拉**：复位默认只有弱上拉（通常 50~100k），10k 压得过；
  而 u-boot 之后该脚是**推挽输出低**，下拉与之同向、不冲突；响时拉高仅多灌 0.33mA，可忽略。

### webd 开机误响（2026-09-30 修复）
- **现象**：每次开机蜂鸣器响**两下**（与"上电长响"是两个独立问题，容易混淆）。
- **原因**：上电 RTC=1970，`quota_slot_index()` 基于假时钟算出假槽位 → 被判为"配额刷新" →
  蜂鸣一次并**把 `used_today` 清零**；NTP 同步后真实槽位又与刚写入的假值不同 → 再响一次。
  副作用：**开机前已消耗的配额被抹掉 = 重启即可绕过每日限额**。
- **修复**：新增 `CtrlState::clock_valid()`（< 2020-01-01 视为不可信）；时钟未同步时
  `refresh_if_due()` 直接返回 false（不刷新 / 不清零 / 不响），等 NTP 校准后再比对真实槽位。
- **验证**：重启后 `grep -c 蜂鸣 /tmp/webd.log` = **0**（修复前为 2）；
  人为把 `last_quota_day` 置 1 再启 webd → 正常响一声、状态文件自动纠正回当前槽位（41454），
  证明刷新蜂鸣功能未被改坏。单测 39 passed（新增 `clock_invalid_blocks_refresh`）。

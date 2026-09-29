# LicheeRV Nano 视觉系统 — 当前进度总结（2026-08-25）

## 一、架构演进对照（三阶段）

| | 阶段 1（旧） | 阶段 2（过渡） | 阶段 3（当前 ✅） |
|---|---|---|---|
| 图像源 | host USB 摄像头 | 板端 OTG 摄像头 | 板端 OTG 摄像头 |
| 传输 | ffmpeg → SSH FIFO | 板端 V4L2 直采 | 板端 V4L2 直采 |
| 解码 | VDEC 硬解 H.264 | （尝试 MJPEG→VDEC 失败） | **JPU 硬解 JPEG**（用户移植驱动） |
| 推理输入 | VDEC NV12 → yuv420_to_nchw | YUYV 直采 → 转换 | JPU NV16 直出 → jpu_nv16_to_nchw |
| 帧率 | 10fps（host 推） | 5fps（限帧） | **10fps（锁定）** |
| 依赖 | host 侧守护脚本 | — | **全板端自包含（S96vision 自启）** |

## 二、当前架构（定稿）

```
UVC 摄像头 (OTG host) /dev/video0, MJPEG 640x480
  → vdec_stream_v4l2 (S96vision 开机自启, 10fps 限帧):
     V4L2 DQBUF → JPU 硬解 (cvi_jpg_test -t 1 -q -ci 1, ioctl 27ms)
     → NV16→NCHW 转换 (15ms) → TPU 推理 yolov8n (29ms)
     → SHM /visiond_detect: detections + MJPEG 原始帧 (10fps)
  → webd (:8080):
     /api/vision/ws     = 检测 JSON (画框驱动)
     /api/vision/frame  = MJPEG 帧直返 (零转码, 同帧缓存)
  → 浏览器: <img> 轮询画面 + canvas 画框
```

**内核模块**（S00kmod 开机加载）：`cv181x_jpeg` + `cvi_vc_driver`（MaxVdec/MaxVencChnNum=9）+ `vc_shim`

## 三、组件状态清单

| 组件 | 状态 | 说明 |
|---|---|---|
| S96vision（vdec_stream_v4l2） | ✅ 运行中 | 板端直采+JPU 硬解+推理，10fps，开机自启 + watchdog 自愈（vdec 卡死心跳 + webd 保活） |
| S90webd（管理台） | ✅ | :8080，视觉/插座/喂食/按键监视；watchdog 保活 + 陈旧帧标记 |
| S90ewelink（插座 daemon） | ✅ | 固件原生 pulse 全插位，token 共享 |
| S98wifiguard（白名单守卫） | ✅ | 10s 四重校验 |
| S99keywipe（按键清配网） | ✅ | 长按 5s → AP 模式 |
| S97wifidisc（发现广播） | ✅ | UDP 37777 供安卓 APP |
| buzzer_beep.sh（配额刷新提醒） | ✅ | A19/GPIO499 低电平触发蜂鸣器模块；用完还原 pinmux（0x4=UART1_RTS）；webd 配额刷新时响 2 下（真机验证 2026-09-09） |
| S97logpersist + crashlog.sh（崩溃日志） | ✅ 运行中 | 把 tmpfs 日志镜像到 `/mnt/data/log/`（messages.log + status.log，各 1MB 封顶），启动横幅判定上次是否正常关机 |
| host super_stream.sh | ⏸ 退役 | 不再使用（曾因 pidof 误匹配导致 VPU 冲突卡死） |

## 四、性能基线（10fps 锁定）

| 指标 | 值 |
|---|---|
| 帧率 | **10.0fps 稳定**（600+ 帧实测；摄像头裸测 17.7fps） |
| 每帧成本 | CPU 侧 ioctl 20ms + io 0ms + conv 8ms(RVV) + fwd 29ms ≈ **57ms** |
| 检测 | person 0.87 稳定检出（实测），dog/cat 同模型 |
| 日志 | 49B/s（降噪 93%），**10MB 环形覆盖**，全程 tmpfs 内存无 SD 写入 |
| 内存 | tmpfs 81.9MB 有界，推理链路零 SD 卡操作 |

## 五、本轮修复的关键问题（含根因）

1. **检测框失踪 / JPU 未真正生效** → `cvi_jpg_test -t 1` 才是解码模式（CVIJPGCOD_DEC=1）；`-t 0` 非法导致 ioctl 失败、静默回退 libjpeg 软解。且 `-t 1` 默认输出 **planar I422**，之前误当 packed-422 读取导致模型输入错乱。已改为 `-t 1 -q -ci 1`（NV16 直出）+ 正确的 `jpu_nv16_to_nchw` ✓
2. **板上 JPU 驱动是旧版** → `/mnt/system/ko/soph_jpeg.ko`（30656B）与本地移植版 `cv181x_jpeg.ko`（31072B）不一致，`-t 1` 直接内核 oops/挂死。已同步新版驱动 ✓
3. **YUYV 索引错误（历史）** → 旧转换 `xx*4` 应为 `xx*2`（每像素 2 字节）+ U/V 组共享
4. **色度双重 -128** → 查表内部已减，调用处不能重复减
5. **部署即失联（VPU 冲突）** → host 守护 pidof 误匹配拉起第二个 vdec 进程抢 VPU，停守护 + S96vision 单进程
6. **卡死（loadavg 虚高）** → USB IO 等待计入，实际 CPU 72% idle
7. **decode_stride 每帧 1.5 万次 expf** → sigmoid 单调，改为只对最大 logits 算一次；`[cls]` 统计同理（score 需赋 sigmoid 值）✓
8. **9.4fps 假象（限帧粒度）** → 摄像头裸测 17.7fps；原 `usleep(10000)` 10ms 步进在处理 ~77ms 后多睡到 ~107ms。改为精确 `usleep(剩余 ms)` 后稳定 10.0fps ✓
9. **USB 摄像头掉线导致控制台"死"** → 掉线后 `/dev/video0` 消失、重连 minor 漂移，vdec_stream 空转 ENODEV。已加自愈：vdec_stream 持续 ENODEV>2s 退出 + 自动扫描 video0..3 + S96vision watchdog 自动重启 ✓
10. **DWC2 控制器卡死规避** → 摄像头缺失 ≥60s 时 watchdog 软复位 DWC2（unbind/bind），仍无效 ≥360s 自动 reboot；避免整机手动重启 ✓
11. **cvi_vc_driver 刷屏/Oops 根治** → 重新编译驱动：注释解码路径 pr_info（dmesg 不再刷屏），`write_yuv_file` 加 NULL 帧缓冲校验（防坏帧 Oops）✓
12. **硬解链路 IO/格式/向量化优化** → ②常驻 fd+mmap（io 0ms）、③驱动 NV12 直出（614→460KB，ioctl 20ms）、⑤RVV 向量化 conv（16→8ms）；dec+scl 46.5→30ms，10fps 稳定 ✓
13. **全链路保活补齐** → S96vision 增加 webd PID/HTTP 保活 + vdec SHM sequence 心跳冻结检测（≥20s 冻结或 SHM 缺失 ≥30s → 先杀 vdec 再 DWC2 复位重启）；webd 增加 stale 标记（>5s 无新帧前端显示"画面停滞"并停止拉旧帧）✓
14. **dmesg 噪声清理** → WiFi AICWFDBG（默认 LOGERROR + insmod 参数）、get_txpwr_max、BT aic_btsdio info、VPSS open/release、VC channel open/close、RTC 周期 time set/read 全部静音；重启后 dmesg 364 行、噪声计数 0 ✓
15. **监控设置页简化** → 合并“插座管理/自动喂食”两张卡去重；固定参数缩为一行；调试模式/按键监视折叠到“高级/调试”；保存反馈改友好文案 ✓
16. **多整点配额刷新** → `refresh_hour` 单点升级为 `refresh_hours` 整点列表；后端按“配额槽索引”在每天每个配置整点重置；设置页改为 24 小时点选 chips；兼容旧状态文件 ✓
17. **配额用尽暂停推理链** → webd 在 `used_today >= quota` 时写 `/tmp/vdec_pause`；默认**全停**（vdec 退出，摄像头/视频一起关，watchdog 不拉起），可配置 `keep_video_on_pause` 保留视频流（vdec 只停 JPU/TPU，继续发实时画面）；配额刷新/清零自动恢复 ✓
18. **调试模式强制恢复链路** → 开启“调试模式（dry_run）”时同时强制开启推理+视频流（清除 pause 标志、watchdog 自动拉起 vdec），方便配额用尽时调试；关闭后按配额状态恢复暂停 ✓
19. **狗窗口写死 1.2s** → 窗口/阈值由 env 可配改为 `control.rs` 代码常量 `DOG_WINDOW_S=1.2` / `DOG_MIN_SCORE=0.65`（`WEB_CTRL_DOG_SECONDS`/`WEB_CTRL_DOG_MIN_SCORE` 废弃，S90webd 不再 export）；此前 `/etc/webd.env` 丢失 `DOG_SECONDS` 导致实际值漂移、文档却写 2s，现已全库统一为 1.2s ✓
20. **基线部署缺口修复** → overlay 里 `buzzer_beep.sh`/`S90webd` 权限 600（不可执行）→ 755；`deploy_overlay.sh` 补入 `S96vision`/`S97wifidisc`/`wifidisc.py`/`buzzer_beep.sh`/`S49ntp`/`ntp.conf` 并统一 chmod + 部署后 `S96vision start` ✓
21. **webd/ewelink-rs 放错目录** → 二者必须在 `/mnt/system/usr/bin/`（`S90webd`/`S90ewelink` 按该路径调用；`/mnt/system` 是 rootfs 普通目录，不是分区）。此前 overlay 放 `usr/bin/`、deploy 也写 `/usr/bin/`，等于历次部署都没更新到正在跑的二进制；已改 overlay 布局 + deploy 目标路径 + 文档 ✓
22. **ewelink daemon 自 08-21 起一直是死的** → `S90ewelink start` 只查 socket 文件，被 -9/断电留下的陈旧 socket 让每次开机都跳过启动，**喂食链路的插座控制整条失效**；已改为进程+socket 双校验（陈旧 socket 先删再拉起），并做 kill -9 回归测试 ✓
23. **崩溃现场日志** → 新增 `S97logpersist`/`crashlog.sh`：tmpfs 日志镜像到 SD、每 60s 状态行、1MB 封顶、启动横幅判定上次是否正常关机（rcK/手动/重启/⚠无记录）✓
24. **部署脚本减负** → 按 md5 跳过未变文件（不再反复写 20MB 的 ewelink-rs），并打印部署前内存/负载快照 ✓

## 六、兼容性说明

- **前端/APP 零改动**：页面接口（frame/ws）、安卓发现 APP 全部沿用
- **webd 保留**：YUYV 转码分支（旧格式）、stream.mjpg 端点保留但前端已不用
- **SHM 契约**：magic@12 区分 JPEG/YUYV，向后兼容
- **主机工具**：deploy_overlay.sh 一键部署仍可用

## 七、待办

- [ ] **观察下一次死机**：2026-09-09 那次死机（7 分钟无响应、硬复位）因日志在 tmpfs 而无定因；现已装 `crashlog`，若复现直接看 `/mnt/data/log/messages.log` + `status.log`
- [ ] 物理排查 USB 摄像头掉线（供电/线材/接口；软件已加 watchdog 自愈兜底）
- [ ] 手机浏览器人工确认画面+画框（需镜头前有猫/狗/人实测）
- [ ] 固件分发前脱敏 ewelink.env / wifi.pass

## 八、关键文件

| 位置 | 内容 |
|---|---|
| tpu-sdk/vdec/src/vdec_stream.c | 板端视觉主程序（V4L2+JPU+TPU） |
| vision-server/webd/ | 管理台（Rust axum，含配额刷新蜂鸣触发） |
| overlay/usr/bin/crashlog.sh + overlay/etc/init.d/S97logpersist | 崩溃现场日志（镜像到 /mnt/data/log） |
| overlay/usr/bin/vdec_stream_v4l2 | 板端二进制（基线） |
| overlay/usr/bin/buzzer_beep.sh | 配额刷新蜂鸣脚本（A19/GPIO499，已入 deploy 清单 + 755） |
| overlay/etc/init.d/S96vision | 视觉自启 + watchdog（vdec 心跳 + webd 保活） |
| overlay/etc/init.d/S90webd | webd 自启 + 手动 stop 标记（watchdog 不抢） |
| docs/knowledge.md | 详细知识库 |

# visiond VENC H.264 直播旁路 — 设计规格

> 2026-08-14
> 关联：2026-07-30-vision-server-design.md（visiond/webd 主设计）、
> 2026-08-14-webd-management-system-plan.md（webd 后台管理）、
> 知识库"TPU/AI 推理"节（VPU boot 间歇性失败教训）
> 状态：规格确认（约束入档），随 visiond 工程化落地

---

## 1. 背景与目标

视觉监控要给客户看**摄像头实时原始画面**（无框或前端叠加框可选）。
现状约束：

| 方案 | 板端 CPU | 720p 带宽@10fps | 结论 |
|------|---------|----------------|------|
| 传原始像素（YUV/RGB） | 0 | **~28MB/s** | RNDIS/WiFi 顶不住，否决 |
| MJPEG（CPU 编码） | 每帧 5-15ms | ~1.5MB/s | 一期可用（640×480） |
| **H.264 硬件编码（VENC）** | **≈0（硬件块）** | **~0.4MB/s** | **二期目标** |

**目标**：visiond 增加 VENC 旁路——AI 推理链不动，H.264 编码走 SG2002
硬件编码器（CVI VENC），满足：

1. **编码放独立线程（异步队列）**——编码卡顿绝不影响推理帧率
2. **ION 内存做预算验证**——128MB 预算内新增 ~6-10MB 有据可依
3. **遵守 VPU 通道关闭纪律**——避免已知 VPU 硬件污染（kill -9 教训）

SDK 依据：`middleware/v2/modules/venc` + `sample/venc`（设备端 `sample_venc` 已部署）；
API 名核对自 `middleware/v2/include/cvi_venc.h` / `cvi_vb.h`。

---

## 2. 总体架构（双线程 + 旁路）

```
                          ┌──────────────────────────── visiond 主线程（不变）───────────────────────────┐
摄像头 GC2053 → ISP ──→ NV12 帧 ──┬──→ CPU 缩放(384×640) ──→ TPU 推理 ──→ DFL+NMS ──→ 检测 JSON ──→ SHM /visiond_detect
                                  │                                                                      │
                                  └──→ 编码队列（环形缓冲 4 槽，非阻塞投递）                                │
                                        │                                                                │
                          ┌───────────▼──────────────┐                                       ┌───────────▼───────────┐
                          │  VENC 编码线程（独立）      │                                       │  webd（Rust，二期）     │
                          │  SendFrame → GetStream     │── H.264 Annex-B 分片 ──(SHM 环形缓冲)──→│  WS /api/video/stream │──→ 浏览器 MSE
                          │  → ReleaseStream           │                                       │  （fMP4 封装）          │
                          └───────────────────────────┘                                       └───────────────────────┘
```

要点：
- **主线程**：采集 → 预处理 → TPU → 解析 → 写 SHM，流程与现设计文档完全一致；
  唯一新增动作 = 把 NV12 帧**非阻塞**投进编码队列（`try_push`，满则覆盖最旧帧）
- **编码线程**：从队列取帧 → `CVI_VENC_SendFrame` → `CVI_VENC_GetStream`（阻塞 ≤200ms）
  → 分片交给 webd → `CVI_VENC_ReleaseStream`；编码慢/失败只丢画面，不阻塞推理
- 检测框**不烧进视频流**：框由前端用 WS 检测 JSON（`/api/vision/ws`）叠加，与视频解耦
- 一期回退：VENC 不可用时 webd `/api/vision/frame`（MJPEG）原样可用，双路共存

---

## 3. 线程模型与数据流（细则）

### 3.1 帧格式

- VENC 输入直接喂 **NV12**（ISP 输出格式），**不做 RGB→NV12 转换**（省一份 CPU 拷贝）
- TPU 输入仍走既有 CPU 缩放路径（NV12→NCHW 查表），两者互不干扰
- VENC 自带缩放器：采集 1080p 时喂全帧，由编码器内部缩到 720p

### 3.2 编码队列

| 项 | 值 | 理由 |
|----|----|------|
| 结构 | 定长环形缓冲（无锁或轻锁） | 避免主线程在编码路径上等待 |
| 容量 | 4 帧 NV12 720p（~6MB） | 吞吐余量 vs ION 预算折中 |
| 满时策略 | **覆盖最旧帧** | 监控场景丢帧优于卡推理（下游只关心最新画面） |
| 投递 | `try_push` 非阻塞，O(1) | 主线程最坏增加 <10μs |

### 3.3 编码线程循环

```c
// 伪代码：编码线程
while (keep_running) {
    frame = queue_pop(timeout=200ms);          // 超时继续循环（空闲时让出）
    if (!frame) continue;
    CVI_VENC_SendFrame(chn, &frame_info, 200); // 硬件接管，立即返回
    CVI_VENC_GetStream(chn, &stream, 200);     // 阻塞等编码完成（≤200ms）
    // stream.pack[].pu8Addr/pu32Len → 组 Annex-B 分片 → 写视频 SHM 环形缓冲 → 通知 webd
    CVI_VENC_ReleaseStream(chn, &stream);
}
```

- `GetStream` 超时 → 计一次 lost，继续下一帧（不重试同帧）
- 线程优先级低于主线程（避免抢占推理）

### 3.4 视频分片通道（visiond → webd）

- 新增 SHM 段 `/visiond_video`（环形缓冲 + sequence，与检测 SHM 同哲学：只写/只读 + 序列号防撕裂）
- 每分片：`sequence u64 | pts_ms u64 | keyframe u8 | len u32 | h264[...]`
- webd 轮询 sequence 变化（复用 `/api/vision/ws` 的 500ms 轮询模式，视频通道 100ms）

---

## 4. VENC 通道配置（推荐参数，落地时按实测微调）

| 参数 | 值 | 说明 |
|------|----|------|
| 编码类型 | `PT_H264` | H.264 Baseline（无 B 帧，低延迟） |
| 分辨率 | 1280×720 | 或跟随采集分辨率（ISP 1080p 由 VENC 内部缩放） |
| 帧率 | **采集 30fps → 抽帧 5fps 推理**（用户决策 2026-08-14：监控场景 5fps 足够） | 推理/编码都只处理采样帧，TPU 负载降 80%+ |
| 码率 | CBR 1.5Mbps | 720p25 低延迟码率；画面复杂可 2Mbps |
| GOP | 30（2s IDR 间隔） | 支持秒级随机接入；异常恢复 `CVI_VENC_RequestIDR` |
| 输入格式 | NV12 | 与 ISP/VDEC 输出一致 |
| 缓冲 | VB pool 2~4 帧（见 §5 预算） | 由 `CVI_VB_CreatePool` 创建，`CVI_VENC_AttachVbPool` 挂接 |

API 清单（已核对 `cvi_venc.h`）：
`CVI_VENC_CreateChn / SetChnAttr / GetChnAttr / StartRecvFrame / StopRecvFrame /
SendFrame / SendFrameEx / GetStream / ReleaseStream / RequestIDR / ResetChn / DestroyChn`；
VB：`CVI_VB_CreatePool / GetBlock / ReleaseBlock / PhysAddr / Handle / DestroyPool`。

---

## 5. ION 内存预算表（75MB carveout 内）

> ⚠️ 数值为**估算**，落地第一步必须以设备实测回填"实测"列
> （工具：`CVI_VB_*` 分配统计 + `/proc/meminfo` + 编解码示例的内存打印）。

| 模块 | 估算 | 实测 | 备注 |
|------|------|------|------|
| VI/ISP 帧缓冲（2 通道 × 4 帧 720p NV12） | 12~18MB | | 与 VENC 帧池可共享 VB pool |
| TPU（模型权重 + 张量，yolov8n 384×640 INT8） | 15~25MB | | 参考：yolov8m 峰值 70-90MB 紧张，v8n 小一个量级 |
| VDEC（测试链路，非实时） | ~6MB | | 实时摄像头链路不占 |
| 其他（ISP 3A/RO/日志/系统预留） | 5~10MB | | |
| **VENC 新增** | **6~10MB** | | 见下 |
| 预留水位 | ≥15MB | | 低于此水位触发告警/降级 |

**VENC 新增明细**：

| 项 | 大小 | 说明 |
|----|------|------|
| 输入帧池 4 帧 NV12 720p | ~6MB | 可与 VI 通道 VB pool 复用（零拷贝送 VENC） |
| bitstream 缓冲 | 2~4MB | 输出码流 + 用户数据缓冲 |
| 内部缩放器/RO | ~0.5MB | VENC 自带 |

**预算验证步骤（落地必做）**：
1. 基线：现有链路满载实测 ION 用量
2. 加 VENC 通道后再测，差值 = VENC 真实成本（预期 ≤10MB）
3. 若超预算：帧池 4→2 帧、码率 1.5→1Mbps、分辨率 720p→640p 依次降级
4. 结果回填本表"实测"列

---

## 6. VPU 通道关闭纪律（强制）

> 依据：知识库实测教训——`VPU_DecOpen failed 0x1`（VDEC boot 间歇失败），
> 同 boot 内重试无效、reboot 才恢复；疑似 kill -9 持有 VPU 通道进程时驱动未清理污染硬件状态。

1. **正常退出**：SIGTERM 处理器 → `StopRecvFrame` → 排空并 `ReleaseStream` 全部 → `DestroyChn` → `CVI_VB_DestroyPool` → 再退出进程
2. **禁止 kill -9** 持有 VENC/VDEC 通道的进程；S90 脚本 stop 一律发优雅信号（`kill -TERM`）
3. **VENC 启动失败**：不重试（同 boot 重试无效，与 VDEC 教训一致）；记录日志并**降级 MJPEG**（webd `/frame` 仍可用），等待重启
4. **启动顺序**：TPU 模型加载成功 → 创建 VENC 通道 → 进主循环；失败按第 3 条处理
5. **测试矩阵（验收必过）**：连续 start/stop ×20 + 冷重启 ×5，无 `VPU_*Open failed`、无画面异常
6. 与 VDEC 链路（视频文件测试）**不同时持有**：测试用 VDEC，实时用 VENC，互斥切换（同一 VPU 硬件 IP）

---

## 7. webd 对接（二期，接口预留）

| 项 | 选择 | 理由 |
|----|------|------|
| 传输 | **WS + MSE（fMP4 分片）** | 浏览器原生支持，无第三方播放器依赖 |
| RTSP | 弃用 | 浏览器不原生支持，需额外播放器库 |
| API | `WS /api/video/stream` | 与 `/api/vision/ws` 并列；`GET /api/video/meta`（分辨率/码率/是否编码中） |
| 封装 | webd 内做 fMP4（moov+mdat 分片，~100 行） | 保持全静态无外部依赖 |
| 帧率控制 | 有帧才推，无帧静默 | 浏览器缓冲自然形成 |

前端：`<video>` + MSE SourceBuffer；检测框仍走 `/api/vision/ws` JSON 叠加（与视频解耦）。
一期（visiond 落地时）：MJPEG `/api/vision/frame` 已可用，接口不变。

---

## 8. 验收标准

1. 720p H.264 编码与 TPU 推理并行：推理帧率 ≥10fps，相对无 VENC 基线**下降 <15%**
2. 板端 CPU：编码期间 CPU 占用增量 <5%（硬编生效的证据）
3. ION：VENC 启用后预留水位 ≥15MB（回填 §5 实测列）
4. 端到端延迟（采集→浏览器显示）≤500ms
5. 连续 start/stop ×20 + 重启 ×5 无 VPU 失败（§6 纪律有效）
6. 浏览器 `<video>` 720p 流畅播放，无花屏/卡死；检测框开关正常（前端叠加）

---

## 9. 分期落地

| 阶段 | 内容 | 依赖 |
|------|------|------|
| 一期（visiond 工程化入库） | MJPEG 640×480 + SHM 现有布局；webd 已就绪 | visiond 源码入库 |
| 二期（本规格） | VENC 旁路 720p H.264 + WS/fMP4 直播 + ION 实测回填 | 一期稳定后 |
| 可选三期 | 码率自适应、录像落盘（/mnt/data）、多路（编码+预览） | 二期验收后 |
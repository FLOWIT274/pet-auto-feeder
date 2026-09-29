# LicheeRV Nano 视觉服务器 — 设计规格

> 2026-07-30
> 项目：LicheeRV Nano (SG2002) + GC2053 摄像头 + NPU 推理 + Axum Web 服务

---

## 1. 背景与目标

在 LicheeRV Nano (SG2002 RISC-V C906B) 上构建一个视觉推理服务器：

- 摄像头实时采集（GC2053, 1920×1080 @30fps, RAW10, 2-lane MIPI）
- NPU 本地推理（cviruntime SDK，替换 dogorcat 项目的 TFLite CPU 推理）
- REST API + WebSocket 对外暴露检测结果
- 为后续 eWelink 智能家居集成预留接入点

**非目标：**

- 不做模型训练（仅推理）
- 不做复杂的视频流编码/推流（仅快照帧输出）
- 不做用户管理/多会话（单用户本地网络服务）
- 不做运行时主核切换（C906B 固定 RISC-V 模式）

---

## 2. 整体架构

采用 **双进程分离架构**，职责明确，独立编译：

```
┌─────────────────────┐     SHM (shm_open)     ┌──────────────────────┐
│    visiond (C)      │ ◄─────────────────────► │    webd (Rust)       │
│                     │                         │                      │
│  ┌───────────────┐  │   struct DetectionShm   │  ┌────────────────┐  │
│  │ V4L2 Camera   │  │   - sequence_num        │  │ Axum HTTP      │  │
│  │ (GC2053)      │──┤   - timestamp_ms        │──┤ - GET /health  │  │
│  └───────┬───────┘  │   - num_detections      │  │ - GET /detect  │  │
│          │ rgb      │   - detections[10]       │  │ - WS /ws       │  │
│          ▼          │   - frame_jpeg_size      │  │ - GET /frame   │  │
│  ┌───────────────┐  │   - frame_jpeg[...]      │  └────────────────┘  │
│  │ NPU Inference │  │                         │                      │
│  │ (cviruntime)  │──┤                         │  ┌────────────────┐  │
│  └───────────────┘  │                         │  │ (预留)         │  │
│                     │                         │  │ eWelink Client │  │
└─────────────────────┘                         └──────────────────────┘
```

| 进程 | 语言 | 职责 |
|------|------|------|
| `visiond` | C99 | 摄像头采集 → 图像预处理 → NPU 推理 → 结果写入共享内存 → JPEG 编码 |
| `webd` | Rust | 读取共享内存 → REST API 输出 → WebSocket 推送 → (将来) eWelink 桥接 |

**为什么选双进程？**

1. **语言各尽其能** — C 调用 cviruntime C API 零开销；Rust 提供内存安全的 Web 服务层
2. **故障隔离** — Web 服务的 panic/OOM 不会拖垮视觉流水线，反之亦然
3. **独立迭代** — 更新 API 无需重新编译 visiond；切换模型/传感器不影响 Rust 层
4. **单线程足够** — visiond 不需要并发（流水线式处理），webd 用 tokio single-thread 也足够

---

## 3. 进程详情：visiond (C)

### 3.1 摄像头模块

**来源：** 复用于 dogorcat 项目的 `camera.c` / `camera.h`（V4L2 API、MMAP 采集、YUYV→RGB 转换）

**适配项：**

- `CAM_CAPTURE_W` / `CAM_CAPTURE_H` — 从 640×480 改为 640×480（不需要全分辨率推理，缩放由 ISP 或软件完成）
- `CAM_INPUT_W` / `CAM_INPUT_H` — 保留 320×320（匹配模型输入）
- GC2053 的驱动已在 buildroot SDK 的 middleware 层（`middleware/v2/component/isp/sensor/cv182x/gcore_gc2053/`），V4L2 层面无需修改
- GC2053 默认 MCLK 为 27MHz，Nano 板使用 24MHz，需在内核驱动中调整 PLL 寄存器
- GC2053 默认 lane_id 为 {1, 3, 2, -1, -1}，需根据 SG2002 Pinmux 确认映射
- MIPI CSI 完整的 ISP 管线由 SDK 的 CIF 驱动处理

**数据流：**

```
GC2053 (MIPI 2-lane) → CIF (MIPI RX) → V4L2 /dev/video0 → MMAP → YUYV(raw) → RGB(320x320) → NPU input tensor
```

> **购买建议：** GC2053 模块约 ¥15-25（淘宝/Lark），显著低于 GC4653（¥35-50）。
> 建议先在 Nano 扩展板上验证 GC2053 出图，再继续 visiond 开发。

### 3.2 NPU 推理模块

**cviruntime C API 替代 TFLite C API：**

| 操作 | TFLite (dogorcat) | cviruntime (本项目) |
|------|-------------------|---------------------|
| 加载模型 | `TfLiteModelCreateFromFile()` | `cvi_npu_init()` / `model_open()` |
| 创建推理会话 | `TfLiteInterpreterCreate()` | `cvi_npu_create_model_handle()` |
| 设置输入 | `TfLiteInterpreterGetInputTensor()` + `memcpy()` | `cvi_npu_set_input_tensor_raw()` |
| 执行推理 | `TfLiteInterpreterInvoke()` | `cvi_npu_run_sync()` |
| 获取输出 | `TfLiteInterpreterGetOutputTensor()` | `cvi_npu_get_output_tensor_raw()` |
| 销毁 | `TfLiteInterpreterDelete()` | `cvi_npu_destroy_model_handle()` + `cvi_npu_deinit()` |

> **注意：** cviruntime C API 的实际函数名以 Sophgo TPU SDK 头文件为准。
> 上表中的函数名为示例占位符，实现时需根据 `cvi_npu.h` 或等效 SDK 头文件确认。

**模型文件：** `.cvimodel` 格式（在开发机上用 nncase 转换 TFLite → cvimodel，部署到板上）

**布局假设：** 输入为 NHWC uint8 [1,320,320,3]，输出为 SSDLite 检测头（scores [1,10], boxes [1,10,4], num_detections [1], classes [1,10]）

### 3.3 JPEG 编码

- 使用轻量级 libjpeg-turbo 或 stb_image.h 中的 `stbi_write_jpg()` 将 RGB 帧编码为 JPEG
- 每次推理后仅保留最新一帧的 JPEG（覆盖写，最多 ~100KB）
- **目的：** `GET /frame` 返回最新快照，避免 Web 层直接访问 V4L2

### 3.4 主循环

```c
while (keep_running) {
    camera_capture_rgb(rgb_frame);            // V4L2 DQBUF → YUYV→RGB
    preprocess_npu(rgb_frame, input_tensor);   // 归一化/布局调整
    cvi_npu_run_sync(model_handle);           // NPU 推理
    parse_output(output_tensors, &detections); // 解析检测头
    encode_jpeg(rgb_frame, &jpeg_buf);         // JPEG 压缩
    write_shm(&detections, jpeg_buf);          // 写入共享内存 + 信号量通知
}
```

目标帧率：**≥10 FPS**（NPU INT8 推理一个 320×320 模型应在 ~30ms 内完成）

---

## 4. 进程详情：webd (Rust)

### 4.1 技术栈

| 组件 | 选择 | 理由 |
|------|------|------|
| 运行时 | tokio `current_thread` | 单核 C906B 无需多线程，节省内存 |
| HTTP 框架 | axum 0.8 | 社区主流，tokio 原生，API 简洁 |
| 共享内存 | shm-rs crate | Rust 绑定 POSIX `shm_open`/`mmap` |
| WebSocket | axum + tokio-tungstenite | Axum 内置 WS 支持 |
| 序列化 | serde + serde_json | Rust 标准 JSON |
| 信号量 | POSIX named semaphore | visiond 写入后 post，webd 等待更新 |

### 4.2 API 设计

```
GET /health
→ 200 {"status":"ok"}

GET /detect
→ 200 {
    "timestamp_ms": 1722345678000,
    "sequence": 42,
    "detections": [
      {"class":"cat","score":0.95,"bbox":[0.1,0.2,0.8,0.7]}
    ]
  }
→ 503 {"error":"no_detection_available"}  (共享内存尚未被写入)

GET /frame
→ 200 image/jpeg  (最新帧的 JPEG 数据)
→ 503  (无可用帧)

WS /ws
→ 每帧检测结果以 JSON 文本帧推送 (同 /detect 格式)
→ 服务器约 10Hz 推送（跟随 visiond 帧率）
```

### 4.3 共享内存读取

```rust
// 映射 visiond 写入的共享内存段
struct DetectionShm {
    sequence_num: u64,        // 递增序号
    timestamp_ms: u64,        // Unix 毫秒时间戳
    num_detections: u32,      // 0~10
    detections: [Detection; 10],
    frame_jpeg_size: u32,     // JPEG 数据长度 (0 = 无帧)
    frame_jpeg: [u8; 131072], // 最大 128KB JPEG
}

struct Detection {
    class_id: u32,    // 0=cat, 1=dog (维持 dogorcat 约定)
    score: f32,       // 置信度 [0,1]
    xmin: f32, ymin: f32, xmax: f32, ymax: f32,  // 归一化 [0,1]
}
```

**读取策略：** 新 HTTP 请求到达时从 SHM 读取当前状态。WebSocket 在独立任务中轮询 SHM 的 `sequence_num`，检测到变更时立即推送。

### 4.4 信号量协调

- **visiond** 完成一帧写入后 `sem_post("/visiond_ready")`
- **webd** 的 WebSocket 后台任务 `sem_wait()` 阻塞等待新帧
- HTTP 请求不使用信号量（直接读取共享内存当前值，无阻塞）

---

## 5. 进程间通信 (IPC)

### 5.1 共享内存段

| 名称 | 大小 | 权限 | 创建者 |
|------|------|------|--------|
| `/visiond_detect` | 约 140KB | 0666 | visiond |
| `/visiond_ready_sem` | POSIX 信号量 | 0666 | visiond |

- visiond 先启动，创建 SHM 和信号量
- webd 后启动，连接已有 SHM

### 5.2 同步保证

- visiond **只写**，webd **只读**
- 写入前先递增 `sequence_num`，写入所有字段后再写 `sequence_num == pending_seq + 1`（序列号既是版本号也是内存屏障的信号）
- webd 先读取 `sequence_num`，再读所有字段，然后验证 `sequence_num` 是否与之前一致——若不一致则重读（检测撕裂读）
- Rust 侧使用 `AtomicU64::load(Ordering::Acquire)` 读取 sequence_num

---

## 6. 构建与部署

### 6.1 项目目录结构

```
/vision-server/
├── Cargo.toml
├── Cargo.lock
├── src/                     # Rust webd
│   ├── main.rs              # Axum 启动
│   ├── shm.rs               # 共享内存读取
│   ├── detection.rs         # 检测结果类型 + JSON serde
│   └── ws.rs                # WebSocket 推送任务
├── visiond/                 # C 子项目
│   ├── CMakeLists.txt
│   ├── src/
│   │   ├── main.c           # 主循环
│   │   ├── camera.c         # V4L2 (复用于 dogorcat)
│   │   ├── camera.h
│   │   ├── detector.c       # NPU 推理 (cviruntime C API)
│   │   ├── detector.h
│   │   ├── shm_writer.c     # 共享内存写入
│   │   └── shm_writer.h
│   └── model/
│       └── detect.cvimodel  # NPU 模型文件
├── build_riscv.sh           # C906B RISC-V 64 交叉编译脚本
└── deploy.sh                # scp + 远程运行
```

**注意：** Rust 侧也需要交叉编译到 riscv64gc-unknown-linux-gnu 目标。需要 riscv64 的 Rust target 和 RISC-V 交叉链接器。

### 6.2 交叉编译流程

**visiond (C)：**
```bash
# 使用 buildroot 的 riscv64-unknown-linux-gnu- 工具链
cmake -DCMAKE_C_COMPILER=riscv64-unknown-linux-gnu-gcc \
      -DCMAKE_C_FLAGS="-march=rv64imafdc -mabi=lp64d -static" \
      -DCMAKE_BUILD_TYPE=MinSizeRel
make -j$(nproc)
```

**webd (Rust)：**
```bash
# 安装 RISC-V target
rustup target add riscv64gc-unknown-linux-gnu

# 需要 RISC-V 交叉链接器 (buildroot 工具链中的 riscv64-unknown-linux-gnu-gcc)
export CC_riscv64gc_unknown_linux_gnu=riscv64-unknown-linux-gnu-gcc
cargo build --target riscv64gc-unknown-linux-gnu --release
```

### 6.3 依赖项

| 依赖 | 用途 | 获取方式 |
|------|------|---------|
| `libcviruntime.so` | NPU 推理 | Sophgo TPU SDK（额外下载） |
| `libjpeg-turbo` 或 `stb_image.h` | JPEG 编码 | stb 单头文件（自带） |
| buildroot toolchain | RISC-V 交叉编译 | `host-tools/`（已在 SDK 内） |
| Rust riscv64 target | Rust 交叉编译 | `rustup target add` |

### 6.4 部署

```bash
# deploy.sh
ssh root@<BOARD_IP> "mkdir -p /opt/vision-server"
scp visiond/visiond root@<BOARD_IP>:/opt/vision-server/
scp target/riscv64gc-unknown-linux-gnu/release/webd root@<BOARD_IP>:/opt/vision-server/
scp model/detect.cvimodel root@<BOARD_IP>:/opt/vision-server/model/
# 远程启动
ssh root@<BOARD_IP> "/opt/vision-server/visiond & /opt/vision-server/webd"
```

---

## 7. 启动顺序与生命周期

```
1.  系统启动 (buildroot init)
2.  加载摄像头驱动 (已在 defconfig 中启用 GC2053)
3.  visiond 启动
    a. camera_init("/dev/video0")
    b. npu_init("/opt/vision-server/model/detect.cvimodel")
    c. shm_create() — 创建共享内存 + 信号量
    d. 进入主循环
4.  webd 启动
    a. shm_attach() — 连接已有共享内存
    b. axum 绑定 0.0.0.0:8080
    c. 接受 HTTP / WebSocket 请求
5.  任一进程退出 → 内核释放共享内存（无孤儿段残留）
6.  visiond 退出时关闭摄像头流 / NPU 句柄
```

---

## 8. 预留扩展点

| 功能 | 影响范围 | 计划迭代 |
|------|---------|---------|
| eWelink 集成 | webd 新增 eWelink HTTP/WS 客户端 | 后续迭代 |
| 多模型切换 | visiond 支持 reload 不同 `.cvimodel` | 后续迭代 |
| 多摄像头 | visiond 管理多个 video 设备 | 后续迭代 |
| 配置热加载 | JSON 配置文件控制参数 | 后续迭代 |
| HTTPS | Axum 支持 TLS | 后续迭代 |

---

## 9. 验收标准

1. ✅ 板子启动后 visiond 能采集 GC2053 图像并成功 NPU 推理
2. ✅ webd 在板子上监听 8080 端口，`curl /detect` 返回 JSON 检测结果
3. ✅ WebSocket 连接后自动推送每帧检测结果
4. ✅ `GET /frame` 返回最新 JPEG 快照
5. ✅ 任一进程崩溃不影响另一进程（SHM 没有死锁）
6. ✅ 推理帧率 ≥10 FPS（端到端：采集→推理→JSON/JPEG→SHM）
7. ✅ 总的 RSS 内存 ≤ 64MB

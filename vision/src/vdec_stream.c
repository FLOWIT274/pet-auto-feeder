// vdec_stream.c — 流式版: FIFO 读 H.264 → VDEC 硬解 → 逐帧 TPU 推理 → 发布 /visiond_detect SHM
// 用法: vdec_stream <model.cvimodel> <h264.fifo> [conf_th] [jpg.fifo]
// 宿主持续写 FIFO; jpg.fifo 每帧推入一张 JPEG (读到 0xFFD9 结束); SIGTERM/SIGINT 优雅退出
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <pthread.h>
#include <unistd.h>
#include <time.h>
#include <math.h>
#include <signal.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/ioctl.h>

#include "cvi_vdec.h"
#include "cvi_sys.h"
#include "cviruntime.h"

#include <setjmp.h>
#include <jpeglib.h>
#if defined(__riscv_vector)
#include <riscv_vector.h>
#endif

#ifndef ALIGN
#define ALIGN(v, a) (((v) + (a) - 1) & ~((a) - 1))
#endif

#define VDEC_CHN 0
#define STREAM_BUF_SIZE ALIGN(1280 * 720, 0x4000)
#define MAX_FRAME_CNT 8

// ---------- SHM 发布 (与 shm_play / visiond_detect 布局一致) ----------
#define SHM_NAME "/visiond_detect"
#define SHM_TOTAL 2000000
#define JPEG_CAP  1474560
// webd 在配额用尽时创建该文件 → vdec 暂停推理链（仍采集+发布实时画面，节省 JPU/TPU）
#define PAUSE_FILE "/tmp/vdec_pause"
// 配额用尽且开启“保留视频流”时同时创建 → 只停推理；否则全停（退出进程，关闭摄像头/视频）
#define KEEP_VIDEO_FILE "/tmp/vdec_keep_video"
static int vdec_paused(void) { return access(PAUSE_FILE, F_OK) == 0; }
static int vdec_keep_video(void) { return access(KEEP_VIDEO_FILE, F_OK) == 0; }
#define MAX_DETS  10
#define SHM_OFF_DET   20                     // detections[10] 起点 (每项 24B)
#define SHM_OFF_JLEN  260                    // 帧长 u32
#define SHM_OFF_JPEG  264                    // 帧数据起点
static int g_shm_fd = -1;
static unsigned char *g_shm = NULL;
static unsigned long long g_seq = 1;

// ---------- 流状态 ----------
static volatile int g_stop = 0;
static long long g_last_infer_ms = 0;   // V4L2 模式限帧
static int g_jpg_fd = -1;
static const char *g_jpg_path = "/tmp/live.jpg.fifo";

static void put_u64(unsigned char *p, unsigned long long v) {
    for (int i = 0; i < 8; i++) p[i] = (unsigned char)(v >> (8 * i));
}
static void put_u32(unsigned char *p, unsigned int v) {
    for (int i = 0; i < 4; i++) p[i] = (unsigned char)(v >> (8 * i));
}
static void put_f32(unsigned char *p, float v) {
    unsigned int u; memcpy(&u, &v, 4); put_u32(p, u);
}
static long long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}
static void stop_handler(int s) { (void)s; g_stop = 1; }

// ---------- V4L2 板端摄像头直采模式 ----------
// argv[2] 为 /dev/video* 时启用: V4L2 抓 MJPG 640x480 → stb 软解 → 居中裁剪 640x360 → TPU 推理
// ⚠️ 本板 DWC2 主机控制器跑不动 YUYV 等时流 (全分辨率实测 0 帧), MJPG 是唯一可用采集格式;
//    选 640x480 是因为模型有效输入恰为 640x360, 裁剪即得, 零缩放
// SHM 存摄像头原始 MJPEG (webd JPEG 模式直通, 板端零编码)
#define V4L2_BUF_TYPE_VIDEO_CAPTURE 1
#define V4L2_MEMORY_MMAP            1
#define VIDIOC_S_FMT     0xC0D05605U
#define VIDIOC_G_FMT     0xC0D05604U
#define VIDIOC_REQBUFS   0xC0145608U
#define VIDIOC_QUERYBUF  0xC0585609U
#define VIDIOC_QBUF      0xC058560FU
#define VIDIOC_DQBUF     0xC0585611U
#define VIDIOC_STREAMON  0x40045612U
#define VIDIOC_STREAMOFF 0x40045613U
#define V4L2_BUFS 4

static int g_v4l2_mode = 0;              // argv[2] 是 /dev/video* 时为 1
static int g_v4l2_fd = -1;
static long long g_last_good_frame_ms = 0;  // 最近一次成功取帧时间 (摄像头丢失检测)
static unsigned char *g_cam_jpeg = NULL; // 最近一帧 MJPEG (供 SHM 发布)
static unsigned int g_cam_jpeg_len = 0;
static pthread_mutex_t g_cam_lock = PTHREAD_MUTEX_INITIALIZER;
static struct { unsigned char *addr; unsigned int len; } g_v4l2_buf[V4L2_BUFS];

// 打开摄像头: S_FMT(MJPG 640x480) → G_FMT 回读校验 → REQBUFS(4) → QUERYBUF+mmap → QBUF → STREAMON
#define CAM_MJPEG_CAP (512 * 1024)
static int v4l2_open(const char *dev)
{
    unsigned char fmt[208], rb[20];
    memset(fmt, 0, sizeof(fmt));
    // RISC-V 64 位: union 8 字节对齐 → v4l2_pix_format 从 +8 开始
    // (+0 type, +8 width, +12 height, +16 pixelformat)
    *(unsigned int *)fmt = 1;                    // type=VIDEO_CAPTURE
    *(unsigned int *)(fmt + 8) = 640;            // width
    *(unsigned int *)(fmt + 12) = 480;           // height
    *(unsigned int *)(fmt + 16) = 0x47504A4D;    // v4l2_fourcc('M','J','P','G')
    if (ioctl(g_v4l2_fd = open(dev, O_RDWR | O_NONBLOCK), VIDIOC_S_FMT, fmt) < 0) {
        perror("[v4l2] S_FMT"); return -1;
    }
    // 回读实际生效格式: 该驱动对不支持的请求会静默回退, 必须校验 (踩过 YUYV 静默变 MJPG 的坑)
    memset(fmt, 0, sizeof(fmt));
    *(unsigned int *)fmt = 1;
    if (ioctl(g_v4l2_fd, VIDIOC_G_FMT, fmt) < 0) {
        perror("[v4l2] G_FMT"); return -1;
    }
    unsigned int gw = *(unsigned int *)(fmt + 8), gh = *(unsigned int *)(fmt + 12);
    unsigned int gpf = *(unsigned int *)(fmt + 16);
    if (gw != 640 || gh != 480 || gpf != 0x47504A4D) {
        printf("[v4l2] 协商结果不符: %ux%u fourcc=%08x\n", gw, gh, gpf);
        return -1;
    }
    memset(rb, 0, sizeof(rb));
    ((unsigned int *)rb)[0] = V4L2_BUFS;
    ((unsigned int *)rb)[1] = 1;
    ((unsigned int *)rb)[2] = 1;
    if (ioctl(g_v4l2_fd, VIDIOC_REQBUFS, rb) < 0) {
        perror("[v4l2] REQBUFS"); return -1;
    }
    for (unsigned int i = 0; i < V4L2_BUFS; i++) {
        unsigned char qb[88];
        memset(qb, 0, sizeof(qb));
        ((unsigned int *)qb)[0] = i;
        ((unsigned int *)qb)[1] = 1;
        if (ioctl(g_v4l2_fd, VIDIOC_QUERYBUF, qb) < 0) {
            perror("[v4l2] QUERYBUF"); return -1;
        }
        unsigned int off = ((unsigned int *)qb)[16];   // +64 m.offset
        unsigned int len = ((unsigned int *)qb)[18];   // +72 length
        g_v4l2_buf[i].addr = (unsigned char *)mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_SHARED, g_v4l2_fd, off);
        g_v4l2_buf[i].len = len;
        if (g_v4l2_buf[i].addr == MAP_FAILED) {
            perror("[v4l2] mmap"); return -1;
        }
        unsigned char q[88];
        memset(q, 0, sizeof(q));
        ((unsigned int *)q)[0] = i;
        ((unsigned int *)q)[1] = 1;
        ((unsigned int *)q)[15] = 1;                   // memory@60
        if (ioctl(g_v4l2_fd, VIDIOC_QBUF, q) < 0) {
            perror("[v4l2] QBUF"); return -1;
        }
    }
    unsigned int ty = 1;
    if (ioctl(g_v4l2_fd, VIDIOC_STREAMON, &ty) < 0) {
        perror("[v4l2] STREAMON"); return -1;
    }
    g_cam_jpeg = (unsigned char *)malloc(CAM_MJPEG_CAP);
    if (!g_cam_jpeg) return -1;
    printf("[v4l2] %s 流启动 (MJPG 640x480, %d bufs)\n", dev, V4L2_BUFS);
    return 0;
}

// 自动扫描摄像头: 指定设备打开失败时, 依次尝试 /dev/video0..3,
// 挑第一个能成功协商 MJPG 640x480 的节点 (解决 USB 重连后 minor 漂移)
static int v4l2_open_auto(const char *prefer)
{
    static const char *cands[] = {
        "/dev/video0", "/dev/video1", "/dev/video2", "/dev/video3", NULL
    };
    // 先试显式指定的设备
    if (prefer && v4l2_open(prefer) == 0) return 0;
    for (int i = 0; cands[i]; i++) {
        if (prefer && strcmp(cands[i], prefer) == 0) continue;
        if (v4l2_open(cands[i]) == 0) {
            printf("[v4l2] 自动选中摄像头: %s\n", cands[i]);
            return 0;
        }
    }
    return -1;
}

// DQBUF 取一帧 → 立即拷贝到 g_cam_jpeg (MJPEG 帧) → QBUF 归还驱动。
// 返回帧字节长 (>0 有帧), -1=无帧/出错。转换必须读 g_cam_jpeg (QBUF 后 mmap 内容归驱动所有!)
static int v4l2_dq(void)
{
    unsigned char dq[88];
    memset(dq, 0, sizeof(dq));
    ((unsigned int *)dq)[1] = 1;                 // type
    ((unsigned int *)dq)[15] = 1;                // memory@60
    if (ioctl(g_v4l2_fd, VIDIOC_DQBUF, dq) < 0) {
        if (errno == EAGAIN) {
            usleep(5000);                        // 无帧, 原链路同款轮询节奏
        } else {                                 // 其他错误限流打印 (每 100 次一条)
            static unsigned err_cnt = 0;
            if (++err_cnt % 100 == 1) printf("[v4l2] DQBUF err=%d (%s)\n", errno, strerror(errno));
        }
        return -1;
    }
    unsigned int idx = ((unsigned int *)dq)[0];
    unsigned int used = ((unsigned int *)dq)[2];
    int ret = -1;
    if (used > 4 && used <= CAM_MJPEG_CAP) {
        pthread_mutex_lock(&g_cam_lock);
        memcpy(g_cam_jpeg, g_v4l2_buf[idx].addr, used);
        g_cam_jpeg_len = used;
        pthread_mutex_unlock(&g_cam_lock);
        ret = (int)used;
    }
    memset(dq, 0, sizeof(dq));
    ((unsigned int *)dq)[0] = idx;
    ((unsigned int *)dq)[1] = 1;
    ((unsigned int *)dq)[15] = 1;
    ioctl(g_v4l2_fd, VIDIOC_QBUF, dq);           // 归还后 mmap 内容不可再读!
    return ret;
}
// ---------- V4L2 模块结束 ----------

// 非阻塞打开 FIFO (轮询等待写端; g_stop 可中断)
static int fifo_open(const char *path)
{
    while (!g_stop) {
        int fd = open(path, O_RDONLY | O_NONBLOCK);
        if (fd >= 0) return fd;
        usleep(200000);
    }
    return -1;
}

// 从 jpg FIFO 读一张完整 JPEG (到 0xFFD9), 返回长度; 阻塞等待
static int jpeg_read_one(unsigned char *out, int cap)
{
    static unsigned char acc[JPEG_CAP + 16];
    static size_t used = 0, scan = 0;
    while (!g_stop) {
        if (g_jpg_fd < 0) {
            g_jpg_fd = fifo_open(g_jpg_path);
            if (g_jpg_fd < 0) return 0;
            used = 0; scan = 0;
        }
        ssize_t n = read(g_jpg_fd, acc + used, sizeof(acc) - used);
        if (n > 0) {
            used += (size_t)n;
            for (; scan + 1 < used; scan++) {
                if (acc[scan] == 0xFF && acc[scan + 1] == 0xD9) {
                    size_t len = scan + 2;
                    if (len > (size_t)cap) len = (size_t)cap;
                    memcpy(out, acc, len);
                    used -= len; scan = 0;
                    memmove(acc, acc + len, used);
                    return (int)len;
                }
            }
            if (used >= sizeof(acc)) { used = 0; scan = 0; }   // 异常数据, 丢弃重来
            continue;
        }
        if (n == 0) { close(g_jpg_fd); g_jpg_fd = -1; usleep(100000); continue; }  // 写端关闭
        if (errno == EAGAIN) { usleep(5000); continue; }
        if (errno == EINTR) continue;
        close(g_jpg_fd); g_jpg_fd = -1; usleep(100000);
    }
    return 0;
}

// 发布一帧到 SHM (惰性初始化; 顺序: ts/num/dets/jpeg/seq 同 shm_play)
// (实现在 Box 定义之后)

static uint64_t g_frame_count = 0;
static uint64_t g_start_ts = 0;
static volatile int g_send_done = 0;
static long g_send_fail = 0;
static volatile int g_resyncing = 0;
static unsigned long g_gf_wait = 0;
static float g_cls_max[3] = {0, 0, 0};

// ---------- 送流线程: FIFO 滚动缓冲, 完整帧才发送 ----------
// 帧边界检测沿用官方逻辑: slice 起始(0x80)与下一帧起点(SPS/PPS/SEI/新slice)
#define ROLL_CAP (2 * 1024 * 1024)
static void *send_stream_thread(void *arg)
{
    const char *path = (const char *)arg;
    if (g_v4l2_mode) {
        // V4L2 直采: MJPEG 帧 → VDEC(PT_MJPEG) 硬解
        unsigned long long pts = 0;
        while (!g_stop) {
            int len = v4l2_dq();
            if (len < 0) continue;
            VDEC_STREAM_S st;
            memset(&st, 0, sizeof(st));
            st.u64PTS = pts++;
            st.pu8Addr = g_cam_jpeg;            // DQBUF 拷入的 MJPEG 帧
            st.u32Len = (unsigned int)len;
            st.bEndOfFrame = CVI_TRUE;
            st.bDisplay = 1;
            if (CVI_VDEC_SendStream(VDEC_CHN, &st, -1) != CVI_SUCCESS) {
                usleep(10000);                  // 积压稍候 (帧数据仍在 g_cam_jpeg)
            }
        }
        printf("[v4l2] send 线程退出\n");
        g_send_done = 1;
        return NULL;
    }
    uint8_t *buf = (uint8_t *)malloc(ROLL_CAP);
    if (!buf) { printf("malloc roll fail\n"); g_send_done = 1; return NULL; }
    size_t used = 0;
    int in_fd = -1;
    uint64_t u64PTS = 0;

    while (!g_stop) {
        if (in_fd < 0) {                       // (重)开 FIFO, 阻塞等差写端
            in_fd = fifo_open(path);
            if (in_fd < 0) break;              // g_stop
            continue;
        }
        if (used == ROLL_CAP) {          // 防御: 异常时丢一半
            memmove(buf, buf + ROLL_CAP / 2, ROLL_CAP / 2);
            used = ROLL_CAP / 2;
        }
        ssize_t n = read(in_fd, buf + used, ROLL_CAP - used);
        if (n > 0) {
            used += (size_t)n;
            // 找第一个 slice 起始
            size_t start = used;
            for (size_t i = 0; i + 8 < used; i++) {
                int tmp = buf[i + 3] & 0x1F;
                if (buf[i] == 0 && buf[i+1] == 0 && buf[i+2] == 1 &&
                    (((tmp == 0x5 || tmp == 0x1) && ((buf[i+4] & 0x80) == 0x80)) ||
                     (tmp == 20 && (buf[i+7] & 0x80) == 0x80))) { start = i; break; }
            }
            // 从 start 后找帧结束 (下一帧起点)
            size_t end = 0;
            for (size_t i = start + 1; i + 8 < used; i++) {
                int tmp = buf[i + 3] & 0x1F;
                if (buf[i] == 0 && buf[i+1] == 0 && buf[i+2] == 1 &&
                    (tmp == 15 || tmp == 7 || tmp == 8 || tmp == 6 ||
                     ((tmp == 5 || tmp == 1) && ((buf[i+4] & 0x80) == 0x80)) ||
                     (tmp == 20 && (buf[i+7] & 0x80) == 0x80))) { end = i; break; }
            }
            if (end == 0) { usleep(10000); continue; }      // 帧不完整, 等更多数据

            // 复位后重新同步: 只从 IDR (NAL 5) 起的完整帧开始发送
            if (g_resyncing) {
                int idr = 0;
                for (; start < end; start++) {
                    if (buf[start] == 0 && buf[start+1] == 0 && buf[start+2] == 1 &&
                        ((buf[start+3] & 0x1F) == 5)) { idr = 1; break; }
                }
                if (idr) g_resyncing = 0;
                else { used = 0; usleep(20000); continue; } // 全丢, 等下个 IDR
            }

            VDEC_STREAM_S stStream;
            memset(&stStream, 0, sizeof(stStream));
            stStream.u64PTS = u64PTS++;
            stStream.pu8Addr = buf;
            stStream.u32Len = (CVI_U32)end;
            stStream.bEndOfFrame = CVI_TRUE;
            stStream.bDisplay = 1;
            if (CVI_VDEC_SendStream(VDEC_CHN, &stStream, -1) != CVI_SUCCESS) {
                if (++g_send_fail > 500) {                 // 连续 5s 失败 → 坏帧, 复位通道自愈
                    printf("[send] SendStream 连续失败 %ld 次, 复位 VDEC + 重同步\n", g_send_fail);
                    fflush(stdout);
                    used = 0; g_resyncing = 1; g_send_fail = 0;
                    CVI_VDEC_StopRecvStream(VDEC_CHN);
                    CVI_VDEC_ResetChn(VDEC_CHN);
                    usleep(20000);
                    CVI_VDEC_StartRecvStream(VDEC_CHN);
                } else {
                    usleep(10000);                          // 短时积压, 稍候重试该块
                }
                continue;                                   // 不推进缓冲, 下轮再试/复位
            }
            g_send_fail = 0;
            g_frame_count++;
            memmove(buf, buf + end, used - end);
            used -= end;
            usleep(10000);               // 官方节奏
            continue;
        }
        if (n == 0) {                    // FIFO 写端关闭 → 重开
            close(in_fd); in_fd = -1;
            usleep(200000);
            continue;
        }
        if (errno == EAGAIN) { usleep(20000); continue; }
        if (errno == EINTR) continue;
        if (in_fd < 0) continue;
        close(in_fd); in_fd = -1;
        usleep(100000);
    }

    // EOS 空包 → 驱动 flush 剩余帧 (关键; 有限尝试避免坏 VDEC 下卡死)
    VDEC_STREAM_S eos;
    memset(&eos, 0, sizeof(eos));
    eos.bEndOfStream = CVI_TRUE;
    for (int i = 0; i < 200; i++) {
        if (CVI_VDEC_SendStream(VDEC_CHN, &eos, -1) == CVI_SUCCESS) break;
        usleep(10000);
    }
    printf("[send] stopped, sent %llu chunks\n", (unsigned long long)g_frame_count);
    fflush(stdout);
    free(buf);
    g_send_done = 1;
    return NULL;
}

// ---------- yolov8 DFL 后处理 ----------
static void softmax16(const float *in, float *out)
{
    float maxv = -1e9f;
    for (int i = 0; i < 16; i++) if (in[i] > maxv) maxv = in[i];
    float sum = 0;
    for (int i = 0; i < 16; i++) { out[i] = expf(in[i] - maxv); sum += out[i]; }
    for (int i = 0; i < 16; i++) out[i] /= sum;
}

typedef struct { float x1, y1, x2, y2, score; int cls; } Box;

static void decode_stride(const int8_t *box_feat, const int8_t *cls_feat,
                          int H, int W, int stride,
                          float box_qscale, float cls_qscale,
                          Box *boxes, int *nboxes, float conf_th)
{
    static const float dfl_w[16] = {0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15};
    for (int y = 0; y < H; y++) {
        for (int x = 0; x < W; x++) {
            // cls: C=3; sigmoid 单调 → 先找原始 logits 最大值, 只对最优类算一次 expf
            int bc = *nboxes;
            float best = -1e9f; int best_c = -1;
            for (int c = 0; c < 3; c++) {
                float s = (float)cls_feat[c * H * W + y * W + x] * cls_qscale;
                if (s > best) { best = s; best_c = c; }
            }
            float sig = 1.0f / (1.0f + expf(-best));
            if (sig > g_cls_max[best_c]) g_cls_max[best_c] = sig;
            if (sig < conf_th || best_c < 0) continue;

            // box: 4x16 DFL (logits 需反量化: int8 * qscale)
            float dist[4];
            for (int k = 0; k < 4; k++) {
                float sm[16], in_f[16];
                for (int i = 0; i < 16; i++)
                    in_f[i] = (float)box_feat[(k * 16 + i) * H * W + y * W + x] * box_qscale;
                softmax16(in_f, sm);
                dist[k] = 0;
                for (int i = 0; i < 16; i++) dist[k] += sm[i] * dfl_w[i];
                dist[k] *= stride;
            }
            float cx = (x + 0.5f) * stride;
            float cy = (y + 0.5f) * stride;
            boxes[bc].x1 = cx - dist[0];
            boxes[bc].y1 = cy - dist[1];
            boxes[bc].x2 = cx + dist[2];
            boxes[bc].y2 = cy + dist[3];
            boxes[bc].score = sig;
            boxes[bc].cls = best_c;
            (*nboxes)++;
        }
    }
}

static void nms(Box *boxes, int n, float iou_th, Box *out, int *nout)
{
    // 简单 NMS:按 score 降序
    for (int i = 0; i < n; i++)
        for (int j = i + 1; j < n; j++)
            if (boxes[j].score > boxes[i].score) {
                Box t = boxes[i]; boxes[i] = boxes[j]; boxes[j] = t;
            }
    *nout = 0;
    for (int i = 0; i < n; i++) {
        int keep = 1;
        for (int j = 0; j < *nout; j++) {
            float ix1 = boxes[i].x1 > out[j].x1 ? boxes[i].x1 : out[j].x1;
            float iy1 = boxes[i].y1 > out[j].y1 ? boxes[i].y1 : out[j].y1;
            float ix2 = boxes[i].x2 < out[j].x2 ? boxes[i].x2 : out[j].x2;
            float iy2 = boxes[i].y2 < out[j].y2 ? boxes[i].y2 : out[j].y2;
            float iw = ix2 - ix1, ih = iy2 - iy1;
            float inter = (iw > 0 && ih > 0) ? iw * ih : 0;
            float area_i = (boxes[i].x2 - boxes[i].x1) * (boxes[i].y2 - boxes[i].y1);
            float area_j = (out[j].x2 - out[j].x1) * (out[j].y2 - out[j].y1);
            if (inter / (area_i + area_j - inter) > iou_th) { keep = 0; break; }
        }
        if (keep) out[(*nout)++] = boxes[i];
    }
}

// 类别顺序: TDL SDK YOLOV8N_DET_PET_PERSON: cat(0), dog(1), person(2)
static const char *CLS_NAMES[3] = {"cat", "dog", "person"};

// ---------- 时域类别确认: 连续 2 次同类别才输出 ----------
// 实测: v8n 在运动/模糊帧上同一目标类别高频振荡 (9.4% 帧对跳变, 含 0.91 高置信错判),
// 投票/阈值均无法压制 → 采用"不确定就静默": 轨迹内连续 2 次检测同类别才保留该框
#define MAX_TRK 16
typedef struct {
    float cx, cy, hw, hh;
    int last_cls;
    int streak;
    uint64_t last_nf;
    int alive;
} Track;
static Track g_trk[MAX_TRK];

// 返回确认后保留的框数 (原地压缩 nms_out, 未确认框被丢弃)
static int stabilize(Box *boxes, int nn, uint64_t nf)
{
    for (int t = 0; t < MAX_TRK; t++) {
        if (g_trk[t].alive && nf - g_trk[t].last_nf > 60) g_trk[t].alive = 0;
    }
    int kept = 0;
    for (int i = 0; i < nn; i++) {
        Box *b = &boxes[i];
        float bcx = (b->x1 + b->x2) / 2, bcy = (b->y1 + b->y2) / 2;
        float bw = b->x2 - b->x1, bh = b->y2 - b->y1;
        int best = -1; float best_d = 1e9f;
        for (int t = 0; t < MAX_TRK; t++) {
            if (!g_trk[t].alive) continue;
            float d = sqrtf((g_trk[t].cx - bcx) * (g_trk[t].cx - bcx) +
                            (g_trk[t].cy - bcy) * (g_trk[t].cy - bcy));
            float th = fmaxf(120.0f, (g_trk[t].hw + g_trk[t].hh + bw + bh) * 0.375f);
            if (d < th && d < best_d) { best = t; best_d = d; }
        }
        int confirm = 0;
        if (best >= 0) {
            Track *t = &g_trk[best];
            if (b->cls == t->last_cls) { t->streak++; confirm = (t->streak >= 2); }
            else { t->streak = 1; }
            t->last_cls = b->cls;
            t->cx = bcx; t->cy = bcy;
            t->hw = bw / 2; t->hh = bh / 2;
            t->last_nf = nf;
        } else {
            for (int t = 0; t < MAX_TRK; t++) {
                if (!g_trk[t].alive) {
                    g_trk[t].alive = 1;
                    g_trk[t].cx = bcx; g_trk[t].cy = bcy;
                    g_trk[t].hw = bw / 2; g_trk[t].hh = bh / 2;
                    g_trk[t].last_nf = nf;
                    g_trk[t].last_cls = b->cls;
                    g_trk[t].streak = 1;
                    break;
                }
            }
        }
        if (confirm) boxes[kept++] = *b;
    }
    return kept;
}

// ---------- SHM 发布 (Box 已定义) ----------
static void publish_frame(const Box *boxes, int nn, uint64_t nf)
{
    (void)nf;
    if (!g_shm) {
        shm_unlink(SHM_NAME);
        g_shm_fd = shm_open(SHM_NAME, O_CREAT | O_EXCL | O_RDWR, 0666);
        if (g_shm_fd < 0) { perror("shm_open"); exit(1); }
        if (ftruncate(g_shm_fd, SHM_TOTAL) != 0) { perror("ftruncate"); exit(1); }
        g_shm = (unsigned char *)mmap(NULL, SHM_TOTAL, PROT_READ | PROT_WRITE, MAP_SHARED, g_shm_fd, 0);
        if (g_shm == MAP_FAILED) { perror("mmap"); exit(1); }
    }
    static unsigned char jpg[JPEG_CAP];
    int jl;
    if (g_v4l2_mode) {                           // V4L2 模式: 存摄像头原始 MJPEG (webd JPEG 直通)
        pthread_mutex_lock(&g_cam_lock);
        jl = (int)g_cam_jpeg_len;
        if (jl > 0 && jl <= JPEG_CAP) memcpy(jpg, g_cam_jpeg, (size_t)jl);
        else jl = 0;
        pthread_mutex_unlock(&g_cam_lock);
    } else {
        jl = jpeg_read_one(jpg, JPEG_CAP);
    }
    int nd = nn > MAX_DETS ? MAX_DETS : nn;
    put_u64(g_shm + 8, (unsigned long long)now_ms());
    put_u32(g_shm + 16, (unsigned int)nd);
    for (int j = 0; j < nd; j++) {
        unsigned int base = 20 + (unsigned int)j * 24;
        put_u32(g_shm + base + 0, (unsigned int)boxes[j].cls);
        put_f32(g_shm + base + 4, boxes[j].score);
        put_f32(g_shm + base + 8, boxes[j].x1);
        put_f32(g_shm + base + 12, boxes[j].y1);
        put_f32(g_shm + base + 16, boxes[j].x2);
        put_f32(g_shm + base + 20, boxes[j].y2);
    }
    put_u32(g_shm + SHM_OFF_JLEN, (unsigned int)jl);
    if (jl > 0) memcpy(g_shm + SHM_OFF_JPEG, jpg, (size_t)jl);
    put_u64(g_shm + 0, g_seq++);
}

// ---------- SEGV 定位 ----------
static volatile uint64_t g_nf_now = 0;
static volatile unsigned g_vf_w = 0, g_vf_h = 0, g_stride0 = 0, g_stride1 = 0;
static volatile unsigned long long g_phy0 = 0;
static volatile unsigned long g_vir0 = 0;
static void segv_handler(int sig)
{
    fprintf(stderr, "SEGV at nf=%llu vf(w=%u h=%u s0=%u s1=%u phy0=%llx vir0=%lx)\n",
            (unsigned long long)g_nf_now, g_vf_w, g_vf_h, g_stride0, g_stride1,
            g_phy0, g_vir0);
    _exit(1);
}

// ---------- CPU 缩放: YUV420 720p → letterbox 384x640 RGB NCHW (int8 0-255) ----------
#define TENSOR_W 640
#define TENSOR_H 384
static uint8_t g_rgb[TENSOR_H * TENSOR_W * 3];
static int16_t g_vt[256], g_ut[256], g_vt2[256], g_ut2[256];  // BT.601 查表 (x1024)

static void yuv_to_rgb_init(void)
{
    for (int i = 0; i < 256; i++) {
        int v = i - 128, u = i - 128;
        g_vt[i]  = (int16_t)(1.402f * v * 1024);   // r
        g_ut[i]  = (int16_t)(-0.344f * u * 1024);  // g
        g_vt2[i] = (int16_t)(-0.714f * v * 1024);  // g
        g_ut2[i] = (int16_t)(1.772f * u * 1024);   // b
    }
}

static void yuv420_to_nchw(const uint8_t *src, const uint8_t *uv_src,
                           int sw, int sh, int yst, int uvst, uint8_t *dst)
{
    // 0.5 缩放: 640x360 有效区, 上下各 pad 12 行; NV12: U/V 交替于 UV 平面
    const int sh2 = 360, pad = 12;
    size_t plane = (size_t)TENSOR_H * TENSOR_W;
    size_t off = 0;
    for (int yy = 0; yy < pad; yy++) {          // 上 pad
        memset(dst + off, 57, TENSOR_W);        // 114>>1
        memset(dst + plane + off, 57, TENSOR_W);
        memset(dst + 2 * plane + off, 57, TENSOR_W);
        off += TENSOR_W;
    }
    for (int yp = 0; yp < sh2; yp++) {          // 有效区 (y 采样 2:1)
        const uint8_t *yrow = src + yp * 2 * yst;
        const uint8_t *uvrow = uv_src + yp * uvst;
        for (int xx = 0; xx < TENSOR_W; xx++) {
            int yv = yrow[xx * 2];
            int col = (xx >> 1) << 1;
            int u = uvrow[col * 2], v = uvrow[col * 2 + 1];
            int r = (yv * 1024 + g_vt[v]) >> 10;
            int g = (yv * 1024 + g_ut[u] + g_vt2[v]) >> 10;
            int b = (yv * 1024 + g_ut2[u]) >> 10;
            // 模型输入语义: uint8/2 (0-127), 位模式即 int8 直接读
            dst[off + xx] = (uint8_t)((r < 0 ? 0 : (r > 255 ? 255 : r)) >> 1);
            dst[plane + off + xx] = (uint8_t)((g < 0 ? 0 : (g > 255 ? 255 : g)) >> 1);
            dst[2 * plane + off + xx] = (uint8_t)((b < 0 ? 0 : (b > 255 ? 255 : b)) >> 1);
        }
        off += TENSOR_W;
    }
    for (int yy = 0; yy < pad; yy++) {          // 下 pad
        memset(dst + off, 57, TENSOR_W);
        memset(dst + plane + off, 57, TENSOR_W);
        memset(dst + 2 * plane + off, 57, TENSOR_W);
        off += TENSOR_W;
    }
}

// ---------- JPU 硬解: 经 cvi_vc_driver 测试钩子 (内核态解码, /dev/shm 中转) ----------
// 依赖板上已加载: cvi_vc_driver.ko(含 cvi_jpg_test) + cv181x_jpeg.ko + vc_shim.ko
#define JPU_IOCTL_MAGIC 'V'
#define CVI_VC_ENC_DEC_JPEG_TEST _IO(JPU_IOCTL_MAGIC, 67)
// JPU 输出 (cvi_jpg_test -t 1 -ci 1 + NV12 直出补丁): Y 平面 640x480 + CbCr 交织 240x640 = 460800B
#define JPU_FRAME_SIZE (480 * 640 + 240 * 640)
static int g_jpu_fd = -1;            // /dev/cvi_vc_enc0; -1=不可用(走 libjpeg 软解兜底)
static int g_fr_jpg_fd = -1;         // /dev/shm/fr.jpg 常驻写 fd (避免每帧 open/close)
static int g_fr_yuv_fd = -1;         // /dev/shm/fr.yuv0.yuv 常驻读 fd
static void *g_fr_yuv_map = NULL;    // 输出 YUV 常驻 mmap (避免每帧 mmap/munmap)
static size_t g_fr_yuv_map_size = 0;

// NV12 (cvi_jpg_test -t 1 -ci 1 + 驱动 NV12 直出补丁): Y 平面 640x480 (stride 640)
// + CbCr 交织平面 240x640 (U@偶, V@奇) → 居中裁剪 640x360 → NCHW
static void jpu_nv12_to_nchw(const uint8_t *src, uint8_t *dst)
{
    const int crop_top = 60;          // (480-360)/2
    const uint8_t *yplane = src;                      // 640x480
    const uint8_t *uvplane = src + (size_t)480 * 640; // 240x640 interleaved CbCr
    size_t plane = (size_t)TENSOR_H * TENSOR_W;
    size_t off = 0;
    for (int yy = 0; yy < 12; yy++) {
        memset(dst + off, 57, TENSOR_W);
        memset(dst + plane + off, 57, TENSOR_W);
        memset(dst + 2 * plane + off, 57, TENSOR_W);
        off += TENSOR_W;
    }
    for (int yp = 0; yp < 360; yp++) {
        // 居中裁剪: 源行 = 60 + yp; NV12 色度行 = 亮度行/2
        const uint8_t *yrow = yplane + (size_t)(yp + crop_top) * 640;
        const uint8_t *uvrow = uvplane + (size_t)((yp + crop_top) >> 1) * 640;
        for (int xx = 0; xx < TENSOR_W; xx += 2) {
            // 420: U/V 每 2 像素共享一组; 直接用定点整数运算(免查表)
            const uint8_t *uvp = uvrow + (size_t)(xx >> 1) * 2;
            int u = uvp[0] - 128, v = uvp[1] - 128;
            int gr = 1436 * v;                     // 1.402*1024
            int gg = -352 * u - 731 * v;           // -0.344*1024, -0.714*1024
            int gb = 1815 * u;                     // 1.772*1024
            for (int k = 0; k < 2; k++) {
                int yv = yrow[xx + k];
                int r = (yv * 1024 + gr) >> 10;
                int g = (yv * 1024 + gg) >> 10;
                int b = (yv * 1024 + gb) >> 10;
                dst[off + xx + k] = (uint8_t)((r < 0 ? 0 : (r > 255 ? 255 : r)) >> 1);
                dst[plane + off + xx + k] = (uint8_t)((g < 0 ? 0 : (g > 255 ? 255 : g)) >> 1);
                dst[2 * plane + off + xx + k] = (uint8_t)((b < 0 ? 0 : (b > 255 ? 255 : b)) >> 1);
            }
        }
        off += TENSOR_W;
    }
    for (int yy = 0; yy < 12; yy++) {
        memset(dst + off, 57, TENSOR_W);
        memset(dst + plane + off, 57, TENSOR_W);
        memset(dst + 2 * plane + off, 57, TENSOR_W);
        off += TENSOR_W;
    }
}

#if defined(__riscv_vector)
// RVV 向量版: 每行先标量展开 U/V 到 640 字节, 再向量算 RGB (定点整数)
static void jpu_nv12_to_nchw_rvv(const uint8_t *src, uint8_t *dst)
{
    const int crop_top = 60;
    const uint8_t *yplane = src;
    const uint8_t *uvplane = src + (size_t)480 * 640;
    size_t plane = (size_t)TENSOR_H * TENSOR_W;
    size_t off = 0;
    for (int yy = 0; yy < 12; yy++) {
        memset(dst + off, 57, TENSOR_W);
        memset(dst + plane + off, 57, TENSOR_W);
        memset(dst + 2 * plane + off, 57, TENSOR_W);
        off += TENSOR_W;
    }
    for (int yp = 0; yp < 360; yp++) {
        const uint8_t *yrow = yplane + (size_t)(yp + crop_top) * 640;
        const uint8_t *uvrow = uvplane + (size_t)((yp + crop_top) >> 1) * 640;
        uint8_t *rrow = dst + off;
        uint8_t *grow = dst + plane + off;
        uint8_t *brow = dst + 2 * plane + off;
        uint8_t uu[640], vv[640];
        for (int xx = 0; xx < 640; xx += 2) {
            uint8_t u = uvrow[(xx >> 1) * 2];
            uint8_t v = uvrow[(xx >> 1) * 2 + 1];
            uu[xx] = u; uu[xx + 1] = u;
            vv[xx] = v; vv[xx + 1] = v;
        }
        for (int xx = 0; xx < 640; xx += 16) {
            size_t vl = vsetvl_e8m1(640 - xx);
            vuint8m1_t y8 = vle8_v_u8m1(yrow + xx, vl);
            vuint8m1_t u8 = vle8_v_u8m1(uu + xx, vl);
            vuint8m1_t v8 = vle8_v_u8m1(vv + xx, vl);
            vint32m4_t y32 = vreinterpret_v_u32m4_i32m4(
                vwcvtu_x_x_v_u32m4(vwcvtu_x_x_v_u16m2(y8, vl), vl));
            vint32m4_t us = vsub_vx_i32m4(vreinterpret_v_u32m4_i32m4(
                vwcvtu_x_x_v_u32m4(vwcvtu_x_x_v_u16m2(u8, vl), vl)), 128, vl);
            vint32m4_t vs = vsub_vx_i32m4(vreinterpret_v_u32m4_i32m4(
                vwcvtu_x_x_v_u32m4(vwcvtu_x_x_v_u16m2(v8, vl), vl)), 128, vl);
            vint32m4_t base = vsll_vx_i32m4(y32, 10, vl);
            // r/g/b = (base + 色差项) >> 10, 再 clamp 0..255, 再 >>1 输出
            vint32m4_t ra = vsra_vx_i32m4(
                vadd_vv_i32m4(base, vmul_vx_i32m4(vs, 1436, vl), vl), 10, vl);
            vint32m4_t ga = vsra_vx_i32m4(
                vadd_vv_i32m4(base,
                    vadd_vv_i32m4(vmul_vx_i32m4(us, -352, vl),
                                   vmul_vx_i32m4(vs, -731, vl), vl), vl),
                10, vl);
            vint32m4_t ba = vsra_vx_i32m4(
                vadd_vv_i32m4(base, vmul_vx_i32m4(us, 1815, vl), vl), 10, vl);
            ra = vmin_vx_i32m4(vmax_vx_i32m4(ra, 0, vl), 255, vl);
            ga = vmin_vx_i32m4(vmax_vx_i32m4(ga, 0, vl), 255, vl);
            ba = vmin_vx_i32m4(vmax_vx_i32m4(ba, 0, vl), 255, vl);
            vse8_v_u8m1(rrow + xx, vnclipu_wx_u8m1(
                vnclipu_wx_u16m2(vreinterpret_v_i32m4_u32m4(ra), 0, vl), 1, vl), vl);
            vse8_v_u8m1(grow + xx, vnclipu_wx_u8m1(
                vnclipu_wx_u16m2(vreinterpret_v_i32m4_u32m4(ga), 0, vl), 1, vl), vl);
            vse8_v_u8m1(brow + xx, vnclipu_wx_u8m1(
                vnclipu_wx_u16m2(vreinterpret_v_i32m4_u32m4(ba), 0, vl), 1, vl), vl);
        }
        off += TENSOR_W;
    }
    for (int yy = 0; yy < 12; yy++) {
        memset(dst + off, 57, TENSOR_W);
        memset(dst + plane + off, 57, TENSOR_W);
        memset(dst + 2 * plane + off, 57, TENSOR_W);
        off += TENSOR_W;
    }
}
#endif

// 成功返回 0 (g_rgb 已填充); -1=失败, 调用方走 libjpeg 兜底
static int jpu_decode_to_nchw(const unsigned char *jpg, int len)
{
    if (g_jpu_fd < 0) return -1;
    // 输入 JPEG: 常驻 fd + ftruncate + pwrite (避免每帧 fopen/fclose)
    if (g_fr_jpg_fd < 0) {
        g_fr_jpg_fd = open("/dev/shm/fr.jpg", O_WRONLY | O_CREAT | O_TRUNC, 0600);
        if (g_fr_jpg_fd < 0) return -1;
    }
    if (ftruncate(g_fr_jpg_fd, 0) != 0) return -1;
    if (pwrite(g_fr_jpg_fd, jpg, len, 0) != (ssize_t)len) return -1;

    char cmd[512];
    snprintf(cmd, sizeof(cmd),
             "cvi_jpg_test -t 1 -q -ci 1 -i /dev/shm/fr.jpg -o /dev/shm/fr.yuv");
    struct timespec a, b, c, d;
    clock_gettime(CLOCK_MONOTONIC, &a);
    if (ioctl(g_jpu_fd, CVI_VC_ENC_DEC_JPEG_TEST, cmd) != 0) return -1;
    clock_gettime(CLOCK_MONOTONIC, &b);
    // 输出 YUV: 常驻 fd + 常驻 mmap (只映射一次, 之后直接读映射页)
    if (g_fr_yuv_map == NULL) {
        g_fr_yuv_fd = open("/dev/shm/fr.yuv0.yuv", O_RDONLY);
        if (g_fr_yuv_fd < 0) return -1;
        struct stat yst;
        if (fstat(g_fr_yuv_fd, &yst) != 0) return -1;
        if (yst.st_size != (off_t)JPU_FRAME_SIZE) return -1;
        g_fr_yuv_map_size = yst.st_size;
        g_fr_yuv_map = mmap(NULL, g_fr_yuv_map_size, PROT_READ, MAP_SHARED,
                            g_fr_yuv_fd, 0);
        if (g_fr_yuv_map == MAP_FAILED) { g_fr_yuv_map = NULL; return -1; }
    }
    clock_gettime(CLOCK_MONOTONIC, &c);
#if defined(__riscv_vector)
    jpu_nv12_to_nchw_rvv((const uint8_t *)g_fr_yuv_map, g_rgb);
#else
    jpu_nv12_to_nchw((const uint8_t *)g_fr_yuv_map, g_rgb);
#endif
    clock_gettime(CLOCK_MONOTONIC, &d);
    static long acc_ioctl = 0, acc_io = 0, acc_conv = 0; static int acc_n = 0;
    acc_ioctl += (b.tv_sec - a.tv_sec) * 1000 + (b.tv_nsec - a.tv_nsec) / 1000000;
    acc_io += (c.tv_sec - b.tv_sec) * 1000 + (c.tv_nsec - b.tv_nsec) / 1000000;
    acc_conv += (d.tv_sec - c.tv_sec) * 1000 + (d.tv_nsec - c.tv_nsec) / 1000000;
    if (++acc_n % 50 == 0)
        printf("[jpu-t] ioctl=%ldms io=%ldms conv=%ldms (avg %d)\n",
               acc_ioctl / acc_n, acc_io / acc_n, acc_conv / acc_n, acc_n);
    return 0;
}

// ---------- MJPEG → RGB888 (libjpeg; 板端固件自带 libjpeg.so.9, 比 stb 快 ~2x) ----------
struct mj_err {
    struct jpeg_error_mgr pub;
    jmp_buf jb;
};
static void mj_error_exit(j_common_ptr ci)
{
    struct mj_err *e = (struct mj_err *)ci->err;
    longjmp(e->jb, 1);
}

// 成功返回 malloc 的 RGB 缓冲 (调用方 free), 失败返回 NULL
static unsigned char *mjpeg_decode_rgb(const unsigned char *buf, unsigned len, int *ow, int *oh)
{
    struct jpeg_decompress_struct cinfo;
    struct mj_err jerr;
    unsigned char *rgb = NULL;
    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = mj_error_exit;
    if (setjmp(jerr.jb)) {                       // libjpeg 错误出口
        jpeg_destroy_decompress(&cinfo);
        free(rgb);
        return NULL;
    }
    jpeg_create_decompress(&cinfo);
    jpeg_mem_src(&cinfo, buf, len);
    jpeg_read_header(&cinfo, TRUE);
    cinfo.out_color_space = JCS_RGB;
    jpeg_start_decompress(&cinfo);
    *ow = cinfo.output_width;
    *oh = cinfo.output_height;
    rgb = (unsigned char *)malloc((size_t)*ow * *oh * 3);
    if (!rgb) { jpeg_destroy_decompress(&cinfo); return NULL; }
    while (cinfo.output_scanline < cinfo.output_height) {
        unsigned char *rp[1] = { rgb + (size_t)cinfo.output_scanline * *ow * 3 };
        jpeg_read_scanlines(&cinfo, rp, 1);
    }
    jpeg_finish_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    return rgb;
}

// ---------- RGB 打包 → 384x640 NCHW: 居中裁剪出 640x360 有效区, 零缩放 ----------
// 输入为 stb 解码的 RGB888 (sw=640, sh=480 → 裁掉上下各 60 行); 宽度不等时最近邻兜底
static void rgb_crop_to_nchw(const uint8_t *src, int sw, int sh, uint8_t *dst)
{
    const int valid_h = TENSOR_H - 24;               // 360 有效行 (上下各 pad 12)
    int crop_top = (sh > valid_h) ? (sh - valid_h) / 2 : 0;
    size_t plane = (size_t)TENSOR_H * TENSOR_W;
    size_t off = 0;
    for (int yy = 0; yy < 12; yy++) {                // 上 pad
        memset(dst + off, 57, TENSOR_W);
        memset(dst + plane + off, 57, TENSOR_W);
        memset(dst + 2 * plane + off, 57, TENSOR_W);
        off += TENSOR_W;
    }
    for (int yp = 0; yp < valid_h; yp++) {
        const uint8_t *row = src + (size_t)(yp + crop_top) * sw * 3;
        for (int xx = 0; xx < TENSOR_W; xx++) {
            const uint8_t *p = row + (sw == TENSOR_W ? xx : xx * sw / TENSOR_W) * 3;
            // 模型输入语义: uint8/2 (0-127), 位模式即 int8 直接读
            dst[off + xx] = p[0] >> 1;
            dst[plane + off + xx] = p[1] >> 1;
            dst[2 * plane + off + xx] = p[2] >> 1;
        }
        off += TENSOR_W;
    }
    for (int yy = 0; yy < 12; yy++) {                // 下 pad
        memset(dst + off, 57, TENSOR_W);
        memset(dst + plane + off, 57, TENSOR_W);
        memset(dst + 2 * plane + off, 57, TENSOR_W);
        off += TENSOR_W;
    }
}

// ---------- 主流程 ----------
int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGSEGV, segv_handler);
    signal(SIGTERM, stop_handler);
    signal(SIGINT, stop_handler);
    signal(SIGPIPE, SIG_IGN);
    yuv_to_rgb_init();
    if (argc < 3) {
        printf("usage: %s <model.cvimodel> <in.h264.fifo|/dev/videoN> [conf_th] [jpg.fifo]\n", argv[0]);
        return 1;
    }
    g_v4l2_mode = (strncmp(argv[2], "/dev/video", 10) == 0);
    if (g_v4l2_mode && v4l2_open_auto(argv[2]) != 0) {
        printf("[v4l2] 摄像头打开失败\n");
        return 1;
    }
    if (g_v4l2_mode) g_last_good_frame_ms = now_ms();
    if (g_v4l2_mode) {
        g_jpu_fd = open("/dev/cvi_vc_enc0", O_RDWR);
        if (g_jpu_fd < 0)
            printf("[jpu] /dev/cvi_vc_enc0 不可用, 走 libjpeg 软解\n");
        else
            printf("[jpu] JPU 硬解通道就绪\n");
    }
    float conf_th = (argc > 3) ? atof(argv[3]) : 0.3f;
    if (argc > 4) g_jpg_path = argv[4];

    // 0. SYS 初始化 (官方 H264: 不配 VB 池, 直接 SYS_Init)
    int sysrc = CVI_SYS_Init();
    printf("SYS_Init rc=%#x\n", sysrc);
    fflush(stdout);

    // 1. TPU 模型加载 (先注册, 避开 VDEC 通道占 ION 后的分配阻塞)
    CVI_MODEL_HANDLE model = NULL;
    printf("RegisterModel...\n"); fflush(stdout);
    int ret = CVI_NN_RegisterModel(argv[1], &model);
    if (ret != CVI_SUCCESS) { printf("RegisterModel fail %d\n", ret); return 1; }
    printf("RegisterModel ok\n"); fflush(stdout);

    CVI_TENSOR *inputs = NULL, *outputs = NULL;
    int32_t in_num = 0, out_num = 0;
    CVI_NN_GetInputTensors(model, &inputs, &in_num);
    CVI_NN_GetOutputTensors(model, &outputs, &out_num);
    printf("in_num=%d out_num=%d\n", in_num, out_num);
    if (in_num != 1 || out_num != 6) {
        printf("unexpected io shape\n"); return 1;
    }

    // 2. 单帧模式优先: 直接读 .rgb 文件 (384x640 NCHW) 推理, 验证模型 (不建 VDEC)
    if (strstr(argv[2], ".rgb")) {
        FILE *fp = fopen(argv[2], "rb");
        if (!fp) { printf("open rgb fail\n"); return 1; }
        size_t rd = fread(g_rgb, 1, sizeof(g_rgb), fp);
        fclose(fp);
        printf("rgb read %zu bytes\n", rd);
        // 输入缩放: 模型期望 uint8/2 (0-127); 可用第3参数覆盖
        float sc = 0.5f;
        if (argc > 3) sc = atof(argv[3]);
        for (size_t i = 0; i < sizeof(g_rgb); i++) {
            unsigned v = (unsigned)(g_rgb[i] * sc);
            g_rgb[i] = (uint8_t)(v > 255 ? 255 : v);
        }
        printf("rgb scaled by %.2f\n", sc);
        CVI_NN_SetTensorPtr(&inputs[0], g_rgb);
        ret = CVI_NN_Forward(model, inputs, in_num, outputs, out_num);
        printf("fwd rc=%d\n", ret);
        for (int i = 0; i < 6; i++) {
            CVI_TENSOR *t = &outputs[i];
            int8_t *d = (int8_t *)t->sys_mem;
            int8_t mn = 127, mx = -128;
            size_t cnt = CVI_NN_TensorCount(t);
            for (size_t k = 0; k < cnt; k++) { if (d[k] < mn) mn = d[k]; if (d[k] > mx) mx = d[k]; }
            printf("  out[%d] %dx%dx%dx%d qscale=%.5f min=%d max=%d\n",
                   i, t->shape.dim[0], t->shape.dim[1], t->shape.dim[2], t->shape.dim[3],
                   t->qscale, mn, mx);
        }
        Box boxes[512], nms_out[512];
        int nb = 0;
        const int strides[3] = {8, 16, 32};
        for (int s = 0; s < 3; s++) {
            CVI_TENSOR *bt = &outputs[s];
            CVI_TENSOR *ct = &outputs[3 + s];
            int H = bt->shape.dim[2], W = bt->shape.dim[3];
            decode_stride((int8_t *)bt->sys_mem, (int8_t *)ct->sys_mem,
                          H, W, strides[s], bt->qscale, ct->qscale,
                          boxes, &nb, conf_th);
        }
        int nn = 0;
        nms(boxes, nb, 0.45f, nms_out, &nn);
        printf("detected %d obj:\n", nn);
        printf("  per-class max conf: cat=%.3f dog=%.3f person=%.3f\n",
               g_cls_max[0], g_cls_max[1], g_cls_max[2]);
        for (int i = 0; i < nn; i++)
            printf("  %s(%.2f)@(%.0f,%.0f,%.0f,%.0f)\n", CLS_NAMES[nms_out[i].cls],
                   nms_out[i].score, nms_out[i].x1, nms_out[i].y1,
                   nms_out[i].x2, nms_out[i].y2);
        CVI_NN_CleanupModel(model);
        return 0;
    }

    // 2.5 VDEC 初始化 (与已修通的 vdec_only3 完全一致的参数; V4L2 模式跳过——JPU 直通)
    VDEC_CHN_ATTR_S chnAttr;
    memset(&chnAttr, 0, sizeof(chnAttr));
    chnAttr.enType = PT_H264;
    chnAttr.enMode = VIDEO_MODE_FRAME;
    chnAttr.u32PicWidth = 1280;
    chnAttr.u32PicHeight = 720;
    chnAttr.u32StreamBufSize = ALIGN(1280 * 720, 0x4000);
    chnAttr.u32FrameBufCnt = 3;

    if (!g_v4l2_mode) {
    ret = CVI_VDEC_CreateChn(VDEC_CHN, &chnAttr);
    if (ret != CVI_SUCCESS) { printf("CreateChn fail %#x\n", ret); return 1; }

    VDEC_CHN_PARAM_S chnParam;
    memset(&chnParam, 0, sizeof(chnParam));
    ret = CVI_VDEC_GetChnParam(VDEC_CHN, &chnParam);
    if (ret != CVI_SUCCESS) { printf("GetChnParam fail %#x\n", ret); return 1; }
    chnParam.enPixelFormat = PIXEL_FORMAT_NV12;
    chnParam.u32DisplayFrameNum = 2;
    ret = CVI_VDEC_SetChnParam(VDEC_CHN, &chnParam);
    if (ret != CVI_SUCCESS) { printf("SetChnParam fail %#x\n", ret); return 1; }

    ret = CVI_VDEC_StartRecvStream(VDEC_CHN);
    if (ret != CVI_SUCCESS) { printf("StartRecvStream fail %#x\n", ret); return 1; }
    printf("VDEC channel ready\n");
    fflush(stdout);
    } // !g_v4l2_mode

    // 3. 送流线程 (V4L2 模式无 VDEC 送流; 采集+JPU 解在主循环)
    pthread_t send_th;
    if (!g_v4l2_mode) pthread_create(&send_th, NULL, send_stream_thread, argv[2]);

    // 4. 取帧 + 推理
    VIDEO_FRAME_INFO_S stVFrame;
    g_start_ts = (uint64_t)time(NULL);
    uint64_t nf = 0;
    while (1) {
        if (g_stop) break;                       // 信号退出 (V4L2 分支无其他出口, 帧流不停则永不检查)
        if (g_v4l2_mode) {                          // ---- V4L2 直采: DQBUF → 转换 → 推理 ----
            long long nowm = now_ms();
            if (nowm - g_last_infer_ms < 100) {
                // 精确补足到 100ms 周期 (不能用 10ms 步进: 处理 ~77ms 后会多睡到 ~107ms → 9.4fps)
                int remain = 100 - (int)(nowm - g_last_infer_ms);
                if (remain > 0) usleep((useconds_t)remain * 1000);
                continue;
            }
            if (v4l2_dq() < 0) {
                if (g_stop) break;
                // 摄像头掉线: 持续 ENODEV/EIO 超过 2s → 退出, 交给 S96vision watchdog 重启
                if ((errno == ENODEV || errno == EIO) &&
                    now_ms() - g_last_good_frame_ms > 2000) {
                    printf("[v4l2] 摄像头丢失 >2s (errno=%d), 退出等 watchdog 重启\n", errno);
                    fflush(stdout);
                    if (g_shm) { munmap(g_shm, SHM_TOTAL); shm_unlink(SHM_NAME); g_shm = NULL; }
                    _exit(3);
                }
                continue;
            }
            g_last_good_frame_ms = now_ms();
            g_last_infer_ms = nowm;                 // 取到帧才占用限帧配额
            // 配额用尽暂停: 默认全停(退出进程关闭摄像头/视频); 开启保留视频流则只停推理
            if (vdec_paused()) {
                if (vdec_keep_video()) {
                    publish_frame(NULL, 0, nf);
                    continue;
                }
                printf("[v4l2] 配额用尽全停: 关闭摄像头/视频, 等配额刷新后由 watchdog 重启\n");
                fflush(stdout);
                if (g_shm) { munmap(g_shm, SHM_TOTAL); shm_unlink(SHM_NAME); g_shm = NULL; }
                _exit(0);
            }
            // 注: 不在这里做 EOI 校验——UVC 帧尾常有 padding, 严格校验会误杀约一半帧。
            // 驱动侧已加 write_yuv_file NULL 帧缓冲防护, 坏帧不再导致 Oops。
            struct timespec tv0, tv1;
            clock_gettime(CLOCK_MONOTONIC, &tv0);
            int jw = 0, jh = 0;
            unsigned char *rgb = NULL;
            if (jpu_decode_to_nchw(g_cam_jpeg, (int)g_cam_jpeg_len) == 0) {
                // JPU 硬解成功
            } else if ((rgb = mjpeg_decode_rgb(g_cam_jpeg, g_cam_jpeg_len,
                                               &jw, &jh)) != NULL) {
                rgb_crop_to_nchw(rgb, jw, jh, g_rgb);
                free(rgb);
            } else {
                printf("[v4l2] JPEG 解码失败 len=%u\n", g_cam_jpeg_len);
                continue;
            }
            clock_gettime(CLOCK_MONOTONIC, &tv1);
            static double acc_s = 0; static int acc_n2 = 0;
            acc_s += (tv1.tv_sec - tv0.tv_sec) * 1000.0 + (tv1.tv_nsec - tv0.tv_nsec) / 1e6;
            if (++acc_n2 % 50 == 0)
                printf("[timing] dec+scl=%.1fms (avg of %d)\n", acc_s / acc_n2, acc_n2);
            g_nf_now = nf;
            goto do_infer;
        }
        memset(&stVFrame, 0, sizeof(stVFrame));
        ret = CVI_VDEC_GetFrame(VDEC_CHN, &stVFrame, 100);   // 100ms 超时, 停机可感知
        if (ret != CVI_SUCCESS) {
            // 检查是否结束
            VDEC_CHN_STATUS_S status;
            if (CVI_VDEC_QueryStatus(VDEC_CHN, &status) == CVI_SUCCESS &&
                status.u32LeftStreamBytes == 0 && status.u32LeftPics == 0 &&
                g_send_done) break;
            if (g_stop && g_send_done && g_gf_wait > 40) {   // 停机但无法排空 → 强制退
                printf("[gf] 停机 5s 仍无法排空 (Left=%u/%u), 强制 _exit\n",
                       status.u32LeftStreamBytes, status.u32LeftPics);
                fflush(stdout);
                if (g_shm) { munmap(g_shm, SHM_TOTAL); shm_unlink(SHM_NAME); }
                _exit(0);
            }
            if (++g_gf_wait % 1000 == 0) {                 // 停滞 ≥100s 可见 (仅正常工作停滞)
                printf("[gf] 停滞 %lu 次: Left=%u/%u send_done=%d 复位=%d\n",
                       g_gf_wait, status.u32LeftStreamBytes, status.u32LeftPics,
                       g_send_done, g_resyncing);
            }
            usleep(1000);
            continue;
        }
        g_gf_wait = 0;

        // CPU 缩放 YUV420 → 384x640 RGB NCHW, 直填输入张量
        g_nf_now = nf;
        g_vf_w = stVFrame.stVFrame.u32Width; g_vf_h = stVFrame.stVFrame.u32Height;
        g_stride0 = stVFrame.stVFrame.u32Stride[0];
        g_stride1 = stVFrame.stVFrame.u32Stride[1];
        g_phy0 = stVFrame.stVFrame.u64PhyAddr[0];
        g_vir0 = (unsigned long)stVFrame.stVFrame.pu8VirAddr[0];
        if (nf % 100 == 0)
        fprintf(stderr, "[gf] nf=%llu phy=%llx/%llx/%llx vir=%lx/%lx/%lx len=%u/%u/%u\n",
                (unsigned long long)nf,
                (unsigned long long)stVFrame.stVFrame.u64PhyAddr[0],
                (unsigned long long)stVFrame.stVFrame.u64PhyAddr[1],
                (unsigned long long)stVFrame.stVFrame.u64PhyAddr[2],
                (unsigned long)stVFrame.stVFrame.pu8VirAddr[0],
                (unsigned long)stVFrame.stVFrame.pu8VirAddr[1],
                (unsigned long)stVFrame.stVFrame.pu8VirAddr[2],
                stVFrame.stVFrame.u32Length[0],
                stVFrame.stVFrame.u32Length[1],
                stVFrame.stVFrame.u32Length[2]);
        if (!stVFrame.stVFrame.pu8VirAddr[0]) {
            printf("frame vir addr NULL\n");
            if (!g_v4l2_mode) CVI_VDEC_ReleaseFrame(VDEC_CHN, &stVFrame);
            break;
        }
do_infer:
        struct timespec t0, t1, t2;
        clock_gettime(CLOCK_MONOTONIC, &t0);
        if (!g_v4l2_mode) yuv420_to_nchw(stVFrame.stVFrame.pu8VirAddr[0],
                       stVFrame.stVFrame.pu8VirAddr[1],
                       stVFrame.stVFrame.u32Width, stVFrame.stVFrame.u32Height,
                       stVFrame.stVFrame.u32Stride[0], stVFrame.stVFrame.u32Stride[1],
                       g_rgb);
        clock_gettime(CLOCK_MONOTONIC, &t1);
        if (nf % 100 == 0)
        fprintf(stderr, "[scl] nf=%llu\n", (unsigned long long)nf);
        ret = CVI_NN_SetTensorPtr(&inputs[0], g_rgb);
        // 首帧模型输入 dump：调试用，默认关闭。
        // 设 VDEC_DUMP_FRAME=1 时写 /tmp（tmpfs）——不写 SD，也不拖慢正常启动。
        // （历史：这里曾无条件写 /mnt/data/frame0.rgb，是排查 YUYV/JPU 转换布局时的临时手段）
        if (nf == 0 && getenv("VDEC_DUMP_FRAME")) {
            FILE *fp = fopen("/tmp/frame0.rgb", "wb");
            if (fp) {
                fwrite(g_rgb, 1, sizeof(g_rgb), fp);
                fclose(fp);
                fprintf(stderr, "[dump] /tmp/frame0.rgb (%zu B)\n", sizeof(g_rgb));
            }
        }
        if (ret != CVI_SUCCESS) {
            printf("SetTensorPtr fail %d\n", ret);
            if (!g_v4l2_mode) CVI_VDEC_ReleaseFrame(VDEC_CHN, &stVFrame);
            break;
        }
        ret = CVI_NN_Forward(model, inputs, in_num, outputs, out_num);
        clock_gettime(CLOCK_MONOTONIC, &t2);
        static double acc_scl = 0, acc_fwd = 0; static int acc_n = 0;
        acc_scl += (t1.tv_sec - t0.tv_sec) * 1000.0 + (t1.tv_nsec - t0.tv_nsec) / 1e6;
        acc_fwd += (t2.tv_sec - t1.tv_sec) * 1000.0 + (t2.tv_nsec - t1.tv_nsec) / 1e6;
        if (++acc_n % 50 == 0)
            printf("[timing] scl=%.1fms fwd=%.1fms (avg of %d)\n",
                   acc_scl / acc_n, acc_fwd / acc_n, acc_n);
        if (nf % 100 == 0)
        fprintf(stderr, "[fwd] nf=%llu\n", (unsigned long long)nf);
        if (ret != CVI_SUCCESS) {
            printf("Forward fail %d\n", ret);
            if (!g_v4l2_mode) CVI_VDEC_ReleaseFrame(VDEC_CHN, &stVFrame);
            break;
        }
        if (!g_v4l2_mode) CVI_VDEC_ReleaseFrame(VDEC_CHN, &stVFrame);

        nf++;
        if (nf % 50 == 0) {
            // cls 张量统计: sigmoid 单调 → 只对全局最大 logits 算一次 expf
            float best_raw = -1e9f; int best_c = -1;
            for (int s = 0; s < 3; s++) {
                CVI_TENSOR *ct = &outputs[3 + s];
                const int8_t *cd = (const int8_t *)ct->sys_mem;
                int H = ct->shape.dim[2], W = ct->shape.dim[3];
                size_t cnt = (size_t)H * W;
                for (size_t i = 0; i < cnt; i++) {
                    float raw = (float)cd[i] * ct->qscale;
                    if (raw > best_raw) { best_raw = raw; best_c = s; }
                }
            }
            float best_cls = 1.0f / (1.0f + expf(-best_raw));
            printf("[cls] nf=%llu best=%.3f cls%d\n",
                   (unsigned long long)nf, best_cls, best_c);
        }
        // 每帧打印检测 (时域稳定化需要高采样率; 之前 10 帧节流导致轨迹断裂)
        if (1) {
            Box boxes[512], nms_out[512];
            int nb = 0;
            // stride 8/16/32 → out[0..2] box, out[3..5] cls
            const int strides[3] = {8, 16, 32};
            for (int s = 0; s < 3; s++) {
                CVI_TENSOR *bt = &outputs[s];
                CVI_TENSOR *ct = &outputs[3 + s];
                int H = bt->shape.dim[2], W = bt->shape.dim[3];
                decode_stride((int8_t *)bt->sys_mem, (int8_t *)ct->sys_mem,
                              H, W, strides[s], bt->qscale, ct->qscale,
                              boxes, &nb, conf_th);
            }
            int nn = 0;
            nms(boxes, nb, 0.45f, nms_out, &nn);
            // 时域类别确认 (连续 2 次同类别才输出, 未确认框丢弃)
            nn = stabilize(nms_out, nn, nf);
            // 坐标修正: 减去顶部 pad 12 行, clip 到有效区
            for (int i = 0; i < nn; i++) {
                nms_out[i].y1 -= 12; nms_out[i].y2 -= 12;
                if (nms_out[i].x1 < 0) nms_out[i].x1 = 0;
                if (nms_out[i].y1 < 0) nms_out[i].y1 = 0;
                if (nms_out[i].x2 > TENSOR_W) nms_out[i].x2 = TENSOR_W;
                if (nms_out[i].y2 > TENSOR_H - 12) nms_out[i].y2 = TENSOR_H - 12;
            }
            uint64_t ts = (uint64_t)time(NULL);
            if (nn > 0) {
                printf("[f=%llu t=%llus] %d obj:", (unsigned long long)nf, (unsigned long long)(ts - g_start_ts), nn);
                for (int i = 0; i < nn; i++) {
                    printf(" %s(%.2f)@(%.0f,%.0f,%.0f,%.0f)", CLS_NAMES[nms_out[i].cls],
                           nms_out[i].score, nms_out[i].x1, nms_out[i].y1,
                           nms_out[i].x2, nms_out[i].y2);
                }
                printf("\n");
            } else if (nf % 100 == 0) {           // no obj 降频到每 100 帧
                printf("[f=%llu t=%llus] no obj\n", (unsigned long long)nf, (unsigned long long)(ts - g_start_ts));
            }
            // 每帧发布到 SHM (webd 视觉卡)
            if (!g_stop) publish_frame(nms_out, nn, nf);
            // 环形日志: 超 10MB 截断到文件头 (stdout 重定向到日志; ftruncate+lseek 保持 fd)
            if (nf % 100 == 0) {
                struct stat lgst;
                if (fstat(STDOUT_FILENO, &lgst) == 0 && lgst.st_size > 10 * 1024 * 1024) {
                    fflush(stdout);                       // 先落盘再截断
                    if (ftruncate(STDOUT_FILENO, 0) == 0) lseek(STDOUT_FILENO, 0, SEEK_SET);
                    fprintf(stderr, "[log] 超 10MB, 已循环覆盖\n");
                }
            }
            if (nf % 100 == 0) {
                double el = (double)(time(NULL) - g_start_ts);
                printf("[live] %llu frames @ %.1f fps\n",
                       (unsigned long long)nf, el > 0 ? (double)nf / el : 0);
            }
        }
    }

    uint64_t ts = (uint64_t)time(NULL);
    uint64_t dt = ts - g_start_ts;
    printf("=== done: %llu frames in %llus = %.1f fps ===\n",
           (unsigned long long)nf, (unsigned long long)dt,
           dt ? (double)nf / dt : 0);
    // SHM 清理 (优雅退出)
    if (g_shm) { munmap(g_shm, SHM_TOTAL); shm_unlink(SHM_NAME); }
    if (g_jpg_fd >= 0) close(g_jpg_fd);
    if (g_fr_yuv_map) munmap(g_fr_yuv_map, g_fr_yuv_map_size);
    if (g_fr_yuv_fd >= 0) close(g_fr_yuv_fd);
    if (g_fr_jpg_fd >= 0) close(g_fr_jpg_fd);

    printf("[cln] exit-path\n"); fflush(stdout);
    if (!g_v4l2_mode) {                          // V4L2 模式无送流线程/VDEC 通道
        printf("[cln] join\n"); fflush(stdout);
        pthread_join(send_th, NULL);
        printf("[cln] stoprecv\n"); fflush(stdout);
        CVI_VDEC_StopRecvStream(VDEC_CHN);
        printf("[cln] reset\n"); fflush(stdout);
        CVI_VDEC_ResetChn(VDEC_CHN);
        printf("[cln] destroy\n"); fflush(stdout);
        CVI_VDEC_DestroyChn(VDEC_CHN);
    }
    printf("[cln] cleanupmodel\n"); fflush(stdout);
    if (model) CVI_NN_CleanupModel(model);
    printf("[cln] exit\n"); fflush(stdout);
    return 0;
}

# JPU 硬解移植 (SG2002 / CV181x)

芯片自带 JPU (0x0B000000, IRQ20)，基线固件未带驱动。本目录是从
sophgo/osdrv `sg200x-dev` 分支移植的产物。

## 板上加载 (重启后需重跑)
```sh
sh /mnt/data/tpu/load_jpu.sh
# 顺序: rmmod soph_vc_driver → soph_jpeg; insmod cv181x_jpeg → vc_shim → cvi_vc_driver
```

## 快速验证
```sh
/mnt/data/tpu/jpu_trigger /dev/cvi_vc_enc0 \
  "cvi_jpg_test -t 1 -q -ci 1 -i /dev/shm/in.jpg -o /dev/shm/out.yuv"
# -t 1 = 解码模式 (CVIJPGCOD_DEC=1); -ci 1 = CbCr 交织 (NV16)
# 输出 NV16: Y 平面 640x480 + CbCr 交织 640x480 = 614400B, 实测 ~26ms/次
# 默认不带 -ci 时输出 planar I422 (Y + Cb + Cr 三平面), 不是 packed-422
```

## bin/
- cv181x_jpeg.ko — 独立 JPU 驱动 (/dev/jpu)。补丁: tWaitQueue 本地化(不再依赖 soph_vcodec)
- vc_shim.ko — 补旧 soph_vcodec 缺失导出: vcodec_lock/trylock/unlock/is_locked, vpu_set_common_memory
- cvi_vc_driver.ko — 新版 VDEC/VENC 字符设备驱动。补丁: 解注释 vdec/venc_vb_ctx 定义、
  无条件启用 CVI_VC_ENC_DEC_JPEG_TEST、编入 cvi_vc_getopt.o

## src/
- 补丁后的源码副本 + gen_stubs_from_kallsyms.sh (modpost 桩表生成)
- 完整可编译树在 osdrv sg200x-dev 原始布局下使用; 单文件副本仅作变更记录

## 管线集成
vdec_stream_v4l2.c: JPU 优先(ioctl /dev/cvi_vc_enc0, /dev/shm 中转), libjpeg 兜底。
启动日志 `[jpu] JPU 硬解通道就绪` = JPU 生效。

#!/bin/sh
# build.sh — 交叉编译 vdec_stream（板端视觉主程序：V4L2 + JPU 硬解 + TPU 推理）
#
# 用法: ./build.sh [输出路径]        默认 bin/vdec_stream_v4l2
#
# 依赖（均为外部 SDK，本仓库不含）：
#   SDK     LicheeRV-Nano-Build 基线 SDK（提供 middleware/v2、linux_5.10、buildroot 产物）
#   TPUSDK  tpu-sdk-sg200x（官方 TPU SDK，提供 include/ 与 lib/*-static.a）
# 二者默认按同级目录查找，也可用环境变量指定：
#   SDK=/path/to/LicheeRV-Nano-Build TPUSDK=/path/to/tpu-sdk ./build.sh
#
# 说明: 本配方可**逐字节复现**已部署的二进制（2026-09-09 校验 md5 一致）。
#       -march=rv64gcv0p7 启用 RVV（jpu_nv12_to_nchw_rvv 用 riscv_vector.h）
set -e
cd "$(dirname "$0")"

OUT="${1:-bin/vdec_stream_v4l2}"
PARENT="$(cd .. && pwd)"
SDK="${SDK:-$PARENT/LicheeRV-Nano-Build}"
TPUSDK="${TPUSDK:-$PARENT/LicheeRV/tpu-sdk/tpu-sdk}"
[ -d "$TPUSDK/include" ] || TPUSDK="$PARENT/tpu-sdk/tpu-sdk"
MW="$SDK/middleware/v2"
LJ="$SDK/buildroot/output/per-package/libjpeg/host/riscv64-buildroot-linux-musl/sysroot/usr"
KAPI="$SDK/linux_5.10/build/sg2002_licheervnano_sd/riscv/usr/include"
CXX="${CXX:-$HOME/toolchains/riscv64-linux-musl-x86_64/bin/riscv64-unknown-linux-musl-g++}"

[ -x "$CXX" ] || { echo "找不到交叉编译器: $CXX（用 CXX=... 指定）"; exit 1; }
[ -d "$MW/include" ] || { echo "缺少 middleware: $MW（用 SDK=... 指定基线 SDK 路径）"; exit 1; }
[ -d "$LJ/include" ] || { echo "缺少 libjpeg sysroot: $LJ（需先构建过基线 SDK 的 buildroot）"; exit 1; }
[ -d "$TPUSDK/include" ] || { echo "缺少 TPU SDK: $TPUSDK（用 TPUSDK=... 指定）"; exit 1; }

mkdir -p "$(dirname "$OUT")"
"$CXX" -O3 -funroll-loops -march=rv64gcv0p7 -o "$OUT" src/vdec_stream.c \
	-I "$TPUSDK/include" \
	-I "$MW/include" \
	-I "$LJ/include" \
	-I "$KAPI" \
	-Wl,--start-group "$MW/lib/libvdec.a" "$MW/lib/libsys.a" "$TPUSDK"/lib/*-static.a -Wl,--end-group \
	-L"$LJ/lib" -ljpeg -latomic -lpthread -lm

ls -l "$OUT"
md5sum "$OUT"

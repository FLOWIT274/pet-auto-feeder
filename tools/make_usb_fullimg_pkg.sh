#!/bin/bash
# make_usb_fullimg_pkg.sh — 把完整 SD 卡镜像 (.img) 打包成 USB 烧录包
#
# 用法: ./make_usb_fullimg_pkg.sh <镜像.img> [输出目录，默认 usb_fullimg_pkg]
#
# 原理：用 raw2cimg.py 把整个 img 打成单个 CIMG (offset=0)，
#       配合 partition_fullimg.xml 直写 TF 卡，产物与 dd 完全一致。
set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# 基线 SDK 路径：优先 config.env 的 SDK，其次环境变量，最后按同级目录推断
[ -f "$SCRIPT_DIR/../config.env" ] && . "$SCRIPT_DIR/../config.env"
BUILD_ROOT="${SDK:-${BUILD_ROOT:-$SCRIPT_DIR/../../LicheeRV-Nano-Build}}"
if [ ! -d "$BUILD_ROOT/build/tools" ]; then
    echo "错误: 找不到基线 SDK: $BUILD_ROOT" >&2
    echo "      请设置 SDK=/path/to/LicheeRV-Nano-Build（或写进 config.env）" >&2
    exit 1
fi
RAW2CIMG_DIR="${BUILD_ROOT}/build/tools/common/image_tool"
USB_DL_DIR="${BUILD_ROOT}/build/tools/cv181x/usb_dl"
FIP_SEARCH_DIRS=(
    "${BUILD_ROOT}/install/soc_sg2002_licheervnano_sd/fip.bin"
    "${BUILD_ROOT}/output/images/fip.bin"
    "${BUILD_ROOT}/fip.bin"
)

# --- 参数解析 ---
if [ $# -lt 1 ]; then
    echo "用法: $0 <镜像.img> [输出目录，默认 usb_fullimg_pkg]"
    exit 1
fi

IMG_PATH="$(realpath "$1")"
OUT_DIR="${2:-${SCRIPT_DIR}/usb_fullimg_pkg}"

if [ ! -f "$IMG_PATH" ]; then
    echo "错误: 镜像文件不存在: $IMG_PATH" >&2
    exit 1
fi

IMG_BASENAME="$(basename "$IMG_PATH")"
IMG_SIZE=$(stat -c%s "$IMG_PATH")
echo "输入镜像: $IMG_PATH ($((IMG_SIZE / 1024 / 1024)) MB)"

# --- 创建输出目录 ---
mkdir -p "$OUT_DIR"
OUT_DIR="$(realpath "$OUT_DIR")"

# --- 查找 fip.bin ---
FIP_PATH=""
for candidate in "${FIP_SEARCH_DIRS[@]}"; do
    if [ -f "$candidate" ]; then
        FIP_PATH="$candidate"
        break
    fi
done
if [ -z "$FIP_PATH" ]; then
    FIP_PATH="$(find "${BUILD_ROOT}/install" -name "fip.bin" -print -quit 2>/dev/null || true)"
fi
if [ -z "$FIP_PATH" ]; then
    echo "警告: 未找到 fip.bin，USB 烧录可能失败" >&2
else
    echo "fip.bin: $FIP_PATH"
fi

# --- 用 raw2cimg.py 生成 CIMG ---
echo "=== 生成 CIMG ==="

# raw2cimg.py 需要 XML 描述分区，且通过 dirname(file_path) 定位文件
# 所以把 img 复制到临时目录，XML 也放在临时目录
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

# size_in_kb: 向上取整 + 1024KB 余量
IMG_SIZE_KB=$(( (IMG_SIZE + 1023) / 1024 ))
PART_SIZE_KB=$((IMG_SIZE_KB + 1024))

# 临时 XML（用 sdcard.img 作为文件名）
cat > "${TMPDIR}/tmp.xml" <<XMLEOF
<physical_partition type="sd">
    <partition label="SDIMG" size_in_kb="${PART_SIZE_KB}" file="sdcard.img" />
</physical_partition>
XMLEOF

cp "$IMG_PATH" "${TMPDIR}/sdcard.img"

# 调用 raw2cimg.py: file_path output_dir xml
python3 "${RAW2CIMG_DIR}/raw2cimg.py" \
    "${TMPDIR}/sdcard.img" \
    "$OUT_DIR" \
    "${TMPDIR}/tmp.xml"

# raw2cimg.py 输出到 ${OUT_DIR}/sdcard.img
CIMG_PATH="${OUT_DIR}/sdcard.img"
CIMG_SIZE=$(stat -c%s "$CIMG_PATH")
CIMG_SIZE_KB=$(( (CIMG_SIZE + 1023) / 1024 ))
CIMG_PART_KB=$((CIMG_SIZE_KB + 1024))

echo "CIMG 已生成: ${CIMG_PATH} (${CIMG_SIZE} bytes)"

# --- 生成 partition_fullimg.xml ---
echo "=== 生成 partition_fullimg.xml ==="
cat > "${OUT_DIR}/partition_fullimg.xml" <<XMLEOF
<physical_partition type="sd">
    <partition label="SDIMG" size_in_kb="${CIMG_PART_KB}" readonly="false" file="sdcard.img" />
</physical_partition>
XMLEOF
echo "partition_fullimg.xml (label=SDIMG, size=${CIMG_PART_KB}KB)"

# --- 复制 fip.bin ---
if [ -n "$FIP_PATH" ]; then
    cp "$FIP_PATH" "${OUT_DIR}/fip.bin"
    echo "fip.bin 已复制"
fi

# --- 复制 USB 烧录工具 ---
echo "=== 复制 USB 烧录工具 ==="
cp "${USB_DL_DIR}/cv181x_dl.py" "$OUT_DIR/"
cp "${USB_DL_DIR}/cv181x_dl.bat" "$OUT_DIR/"

if [ -d "${USB_DL_DIR}/rom_usb_dl" ]; then
    cp -r "${USB_DL_DIR}/rom_usb_dl" "$OUT_DIR/"
    echo "rom_usb_dl/ 已复制"
fi
if [ -d "${USB_DL_DIR}/utils" ]; then
    cp -r "${USB_DL_DIR}/utils" "$OUT_DIR/"
    echo "utils/ 已复制"
fi

# --- 完成 ---
echo ""
echo "=========================================="
echo "烧录包已生成: ${OUT_DIR}/"
echo "=========================================="
echo ""
echo "文件列表:"
ls -lh "$OUT_DIR/"
echo ""
echo "=== 烧录命令 ==="
echo "  cd ${OUT_DIR}"
echo "  python3 cv181x_dl.py              # 默认方式"
echo "  python3 cv181x_dl.py --libusb     # Linux 推荐（需 root）"
echo "  python3 cv181x_dl.bat             # Windows"
echo ""
echo "注意:"
echo "  1. 板子按住 BOOT 键上电进入 ROM USB 模式"
echo "  2. USB2.0 写入约 $((CIMG_PART_KB / 1024 / 1024))MB 需 30-45 分钟"
echo "  3. 烧录完成后拔卡重新上电即可启动"

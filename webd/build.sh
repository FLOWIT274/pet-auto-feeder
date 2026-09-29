#!/bin/sh
# webd 交叉编译脚本（riscv64gc-unknown-linux-musl，全静态）
# 用法：./build.sh [host|riscv]
#   host  — 宿主构建（开发调试，默认）
#   riscv — 设备端 RISC-V 静态二进制
set -e
cd "$(dirname "$0")"

case "${1:-host}" in
	host)
		cargo build --release
		echo "宿主二进制: target/release/webd"
		;;
	riscv)
		rustup target add riscv64gc-unknown-linux-musl 2>/dev/null || true
		export CC_riscv64gc_unknown_linux_musl="${CC_riscv64gc_unknown_linux_musl:-$HOME/toolchains/riscv64-linux-musl-x86_64/bin/riscv64-unknown-linux-musl-gcc}"
		export AR_riscv64gc_unknown_linux_musl="${AR_riscv64gc_unknown_linux_musl:-$HOME/toolchains/riscv64-linux-musl-x86_64/bin/riscv64-unknown-linux-musl-ar}"
		cargo build --release --target riscv64gc-unknown-linux-musl
		BIN=target/riscv64gc-unknown-linux-musl/release/webd
		echo "设备二进制: $BIN ($(ls -lh "$BIN" | awk '{print $5}'))"
		;;
	*)
		echo "用法: $0 [host|riscv]"; exit 1
		;;
esac
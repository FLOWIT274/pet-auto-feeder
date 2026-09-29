#!/bin/sh
# buzzer_beep.sh — 配额刷新提醒蜂鸣器
#
# 硬件: 低电平触发的蜂鸣器模块; GPIO 只作触发信号, 不驱动负载(模块自带驱动电路)
# 触发脚: A19 = GPIOA19 = pad JTAG_CPU_TMS = Linux GPIO 499
#   FMUX 寄存器 0x03001064 (3bit 字段, 该寄存器仅此脚使用):
#     0=JTAG_CPU_TMS  1=CAM_MCLK0  2=PWM_7  3=XGPIOA_19  4=UART1_RTS ...
#   u-boot 开机把该脚置为 UART1_RTS(0x4)(蓝牙流控, 本项目未用);
#   本脚本临时切 GPIO(0x3) → 鸣叫 → 还原(0x4), 不长期占用该 pad。
#
# 用法: buzzer_beep.sh [次数]          默认 2 下
#       RESTORE=0 buzzer_beep.sh 2     响完保持 GPIO 复用(调试用, 默认 1=还原)
# 依赖: devmem(切 pinmux), sysfs gpio

GPIO=499
PINMUX_REG=0x03001064
MUX_GPIO=0x03	# XGPIOA_19
MUX_BOOT=0x04	# UART1_RTS = u-boot 开机默认复用
RESTORE=${RESTORE:-1}
BEEP=${1:-2}
ON=0.15		# 单次响时长(秒)
GAP=0.25	# 两次之间间隔(秒)

set_out() { echo "$1" > "/sys/class/gpio/gpio$GPIO/value" 2>/dev/null; }

# 退出/被打断(webd 调用时有 5s 超时 kill)时: 先回高=关闭, 再还原开机默认复用
cleanup() {
	set_out 1
	if [ "$RESTORE" = "1" ]; then
		devmem "$PINMUX_REG" 32 "$MUX_BOOT" 2>/dev/null
	fi
}
trap 'cleanup; exit 130' INT TERM HUP

# 1) 切到 GPIO 功能
devmem "$PINMUX_REG" 32 "$MUX_GPIO" 2>/dev/null

# 2) 导出 + 方向输出 + 默认高电平(关闭), 避免执行瞬间误响
[ -d "/sys/class/gpio/gpio$GPIO" ] || echo "$GPIO" > /sys/class/gpio/export 2>/dev/null
echo out > "/sys/class/gpio/gpio$GPIO/direction" 2>/dev/null
set_out 1

# 3) 鸣叫: 低电平触发 → 保持 ON → 回高关闭 → 间隔 GAP
i=0
while [ "$i" -lt "$BEEP" ]; do
	set_out 0
	sleep "$ON"
	set_out 1
	i=$((i + 1))
	if [ "$i" -lt "$BEEP" ]; then
		sleep "$GAP"
	fi
done

# 4) 收尾(回高 + 还原复用)
cleanup

#!/bin/sh
# buzzer_beep.sh — 配额刷新提醒蜂鸣器
#
# 硬件: **高电平触发**的蜂鸣器模块（自带驱动电路，GPIO 只作触发信号，不驱动负载）
#   → 静音(idle) = 低电平(0)；鸣叫(active) = 高电平(1)
#   触发脚: A19 = GPIOA19 = pad JTAG_CPU_TMS = Linux GPIO 499
#   FMUX 寄存器 0x03001064 (3bit 字段, 该寄存器仅此脚使用):
#     0=JTAG_CPU_TMS  1=CAM_MCLK0  2=PWM_7  3=XGPIOA_19  4=UART1_RTS ...
#   开机状态：u-boot 已改为把该脚设成 **XGPIOA_19 输出低电平（静音）**
#     （见 patches/01 的 cvi_board_init.c；原厂是 UART1_RTS，会让高触发模块长响）。
#
# 用法: buzzer_beep.sh [次数]          默认 2 下
#       RESTORE_MUX=1 buzzer_beep.sh 2 响完把 pinmux 还原成 UART1_RTS（默认 0，见下）
#       ACTIVE_HIGH=0 buzzer_beep.sh 2 若模块实为低电平触发，用它翻转极性
# 依赖: devmem(切 pinmux), sysfs gpio
#
# ⚠️ 为什么默认 RESTORE_MUX=0（响完保持 GPIO 输出低，不还原成 UART1_RTS）：
#   UART1_RTS 在 MCR 复位值(0)下是去断言态，16550 类 UART 的 RTS 低有效
#   → 该 pad 被驱动为**高**。若模块是高触发，一还原就**持续长响**；
#   即使模块是低触发，保持 GPIO 输出也能给出确定的静音电平。
#   代价：该 pad 长期占为 GPIO（不再作 JTAG TMS / UART1 RTS）。
#
# ⚠️ 极性：现场口述曾出现"低触发/高触发"两种说法（2026-09-30）。实测依据：
#   板子原厂 A19=UART1_RTS 且 MCR=0（引脚为高）时，若模块为高触发会持续响。
#   现按**高电平触发**实现（ACTIVE_HIGH=1）。若实测发现"该响时不响、平时反而响"，
#   设 ACTIVE_HIGH=0 即可翻转，无需改代码。

GPIO=499
PINMUX_REG=0x03001064
MUX_GPIO=0x03	# XGPIOA_19
MUX_BOOT=0x04	# UART1_RTS = 原厂 u-boot 默认复用
RESTORE_MUX=${RESTORE_MUX:-0}
ACTIVE_HIGH=${ACTIVE_HIGH:-1}
BEEP=${1:-2}
if [ "$ACTIVE_HIGH" = "1" ]; then
	IDLE=0; ACTIVE=1	# 高触发模块: 低=静音, 高=鸣叫
else
	IDLE=1; ACTIVE=0	# 低触发模块: 高=静音, 低=鸣叫
fi
ON=0.15		# 单次响时长(秒)
GAP=0.25	# 两次之间间隔(秒)

set_out() { echo "$1" > "/sys/class/gpio/gpio$GPIO/value" 2>/dev/null; }

# devmem 在 /usr/sbin，init 脚本的 PATH 未必包含它 → 用绝对路径，找不到再退回 PATH
devmem_bin() { [ -x /usr/sbin/devmem ] && echo /usr/sbin/devmem || echo devmem; }

# 退出/被打断(webd 调用时有 5s 超时 kill)时: 先回静音电平, 再按需还原复用
cleanup() {
	set_out "$IDLE"
	if [ "$RESTORE_MUX" = "1" ]; then
		"$(devmem_bin)" "$PINMUX_REG" 32 "$MUX_BOOT" 2>/dev/null
	fi
}
trap 'cleanup; exit 130' INT TERM HUP

# 1) 切到 GPIO 功能
"$(devmem_bin)" "$PINMUX_REG" 32 "$MUX_GPIO" 2>/dev/null

# 2) 导出 + 设为输出并直接给静音电平
#    用 "low"（Linux 5.10 里等价于 "out"：gpiod_direction_output_raw(desc,0)）
#    —— 切到 GPIO 的瞬间就是静音电平，不会产生"先响一下"的毛刺。
#    （若写成 "high" 反而会先拉高 → 高触发模块立刻响，故此处必须用 low）
[ -d "/sys/class/gpio/gpio$GPIO" ] || echo "$GPIO" > /sys/class/gpio/export 2>/dev/null
echo low > "/sys/class/gpio/gpio$GPIO/direction" 2>/dev/null
set_out "$IDLE"

# 3) 鸣叫: 拉高触发 → 保持 ON → 回低静音 → 间隔 GAP
i=0
while [ "$i" -lt "$BEEP" ]; do
	set_out "$ACTIVE"
	sleep "$ON"
	set_out "$IDLE"
	i=$((i + 1))
	if [ "$i" -lt "$BEEP" ]; then
		sleep "$GAP"
	fi
done

# 4) 收尾(回静音 + 按需还原复用)
cleanup

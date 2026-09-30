#!/bin/sh
# crashlog.sh — 把易失日志镜像到 SD，供死机后定因
#
# 背景：/var/log 是指向 /tmp 的软链（tmpfs），重启后 dmesg / webd / vdec 日志全丢。
# 2026-09-09 一次疑似死机（板子 7 分钟无响应、硬复位）因此无法定因。本脚本提供最小代价的持久现场：
#   /mnt/data/log/messages.log  镜像 /var/log/messages（klogd 内核消息 + syslog 用户消息）
#   /mnt/data/log/status.log    每 60s 一行运行状态（内存/swap/负载/WiFi/视觉序号；序号冻结时附 vdec 日志尾部）
# 两个文件各自 1MB 封顶（超限保留最后 256KB，原地截断以保持 inode，tail 仍能续写）。
# 正常写入量：状态行 ~150B/分钟；messages 仅在 syslog 有新行时追加（dmesg 噪声已清理）。
#
# 用法: crashlog.sh start|stop|status   （由 /etc/init.d/S97logpersist 调用）

DIR=/mnt/data/log
LOG=$DIR/messages.log
STATUS=$DIR/status.log
PIDFILE=/var/run/crashlog.pid
TAILPID=/var/run/crashlog.tail.pid
CAP=1048576		# 单文件上限 1MB
KEEP=262144		# 超限后保留最后 256KB
INTERVAL=60

cap_file() {
	[ -f "$1" ] || return 0
	sz=$(wc -c < "$1" 2>/dev/null || echo 0)
	[ "$sz" -gt "$CAP" ] || return 0
	# 原地截断（同一 inode），否则 tail 的 fd 指向被替换的旧 inode，之后日志全丢
	tail -c "$KEEP" "$1" > "$1.cap" 2>/dev/null && cat "$1.cap" > "$1" && rm -f "$1.cap"
}

wifi_info() {
	st=$(cat /sys/class/net/wlan0/operstate 2>/dev/null)
	sig=""
	if [ -r /proc/net/wireless ]; then
		sig=$(awk '/wlan0/{print " link="$3" lvl="$4}' /proc/net/wireless 2>/dev/null)
	fi
	echo "wlan0=${st:-?}${sig}"
}

vision_seq() {
	od -An -tu8 -j 0 -N 8 /dev/shm/visiond_detect 2>/dev/null | tr -d ' '
}

snapshot() {
	ts=$(date "+%Y-%m-%d %H:%M:%S")
	up=$(cut -d. -f1 /proc/uptime)
	mem=$(free | awk '/^Mem:/{print "used="$3"K avail="$7"K"}')
	swp=$(free | awk '/^Swap:/{print "swap="$3"/"$2"K"}')
	load=$(cut -d' ' -f1-3 /proc/loadavg)
	seq=$(vision_seq)
	echo "$ts up=${up}s $mem $swp load=$load $(wifi_info) seq=${seq:-none}" >> "$STATUS"

	# 视觉序号连续 3 次不变 → 疑似卡死，附 vdec 日志尾部作现场
	if [ -n "$seq" ] && [ "$seq" = "$last_seq" ]; then
		stall=$((stall + 1))
	else
		stall=0
	fi
	last_seq=$seq
	if [ "$stall" -ge 3 ]; then
		echo "  !! 视觉序号冻结 ${stall}0s+, vdec 日志尾部:" >> "$STATUS"
		tail -5 /tmp/vdec_v4l2.log 2>/dev/null | sed 's/^/  | /' >> "$STATUS"
		stall=0
	fi
}

start() {
	mkdir -p "$DIR"
	# 若已有实例在跑，这属于"重启"：先记一笔，免得下次被误判成"未正常关机"
	if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
		echo "restart $(date '+%Y-%m-%d %H:%M:%S')" > "$DIR/.last-stop"
	fi
	kill_procs		# 注意：不能用 stop()，否则会写下"正常关机"标记

	ts=$(date "+%Y-%m-%d %H:%M:%S")
	# 上次是怎么结束的？stop() 会写 .last-stop（区分 rcK 关机 / 手动停止）；
	# 没有记录 = 断电或死机（再用 dmesg 佐证）
	clean=""
	if [ -f "$DIR/.last-stop" ]; then
		kind=$(cut -d' ' -f1 "$DIR/.last-stop")
		when=$(cut -d' ' -f2- "$DIR/.last-stop")
		case "$kind" in
			shutdown) clean=" 上次正常关机 ($when)" ;;
			manual)   clean=" 上次为手动停止 ($when)" ;;
			restart)  clean=" 上次为进程重启 ($when)" ;;
			*)        clean=" 上次停止方式未知 ($when)" ;;
		esac
		rm -f "$DIR/.last-stop"
	else
		ev=""
		dmesg 2>/dev/null | grep -q "not properly unmounted" && ev="$ev 未干净卸载(/boot FAT)"
		dmesg 2>/dev/null | grep -q "recovery complete" && ev="$ev ext4 日志恢复"
		clean=" ⚠ 上次未正常关机（无停止记录${ev:+；}$ev）"
	fi

	# 每次"开机"只补齐一次 syslog（含开机内核消息）；同一开机内重复 start 只记一行，避免重复灌入
	BOOTID=$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)
	[ -n "$BOOTID" ] || BOOTID=$(cut -d' ' -f1 /proc/stat)
	STATE=/var/run/crashlog.bootid
	if [ "$(cat "$STATE" 2>/dev/null)" != "$BOOTID" ]; then
		echo "=== boot $ts$clean ===" >> "$LOG"
		[ -s /var/log/messages ] && cat /var/log/messages >> "$LOG" 2>/dev/null
		echo "$BOOTID" > "$STATE"
	else
		echo "=== restart $ts (同一开机)$clean ===" >> "$LOG"
	fi
	cap_file "$LOG"

	tail -f /var/log/messages >> "$LOG" 2>/dev/null &
	echo $! > "$TAILPID"

	(
		stall=0
		last_seq=""
		while true; do
			sleep "$INTERVAL"
			snapshot
			cap_file "$STATUS"
			cap_file "$LOG"
		done
	) >/dev/null 2>&1 &
	echo $! > "$PIDFILE"
	echo "crashlog: 已启动 (pid $(cat "$PIDFILE")) → $DIR"
}

kill_procs() {
	[ -f "$PIDFILE" ] && kill "$(cat "$PIDFILE")" 2>/dev/null
	[ -f "$TAILPID" ] && kill "$(cat "$TAILPID")" 2>/dev/null
	rm -f "$PIDFILE" "$TAILPID"
}

stop() {
	kill_procs
	mkdir -p "$DIR"
	# 记录"上次是怎么停的"：rcK（关机流程）→ shutdown；其它（手动/部署）→ manual。
	# 只在关机路径写 shutdown，避免手动 stop 被误判成"正常关机"而掩盖真正的死机。
	#
	# 注意：crashlog.sh 一般由 /etc/init.d/S97logpersist 调起，调用链是
	#   rcK → S97logpersist → crashlog.sh
	# 所以 $PPID 是 S97logpersist 而**不是** rcK。早先只查 $PPID，导致 reboot/关机
	# 一律被误记成 manual（2026-09-30 排查无限重启时因此误判为"有人在手动重启"）。
	# 这里沿 /proc/<pid>/stat 的 ppid 字段向上回溯若干层再判定。
	kind=manual
	pid=$PPID
	i=0
	while [ "$i" -lt 6 ]; do
		case "$pid" in ''|*[!0-9]*) break ;; esac
		[ "$pid" -le 1 ] && break
		cmd=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)
		case "$cmd" in *rcK*) kind=shutdown; break ;; esac
		# comm 字段在括号内且可能含空格, 故先剥掉到最后一个 ')' 再取 state 后的 ppid
		pid=$(sed 's/^[^)]*) //' "/proc/$pid/stat" 2>/dev/null | cut -d' ' -f2)
		i=$((i + 1))
	done
	echo "$kind $(date '+%Y-%m-%d %H:%M:%S')" > "$DIR/.last-stop"
}

status() {
	if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
		echo "crashlog: 运行中 (pid $(cat "$PIDFILE"))"
	else
		echo "crashlog: 未运行"
	fi
	ls -l "$LOG" "$STATUS" 2>/dev/null
}

case "$1" in
	start) start ;;
	stop) stop ;;
	status) status ;;
	*) echo "用法: $0 start|stop|status"; exit 1 ;;
esac

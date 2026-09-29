#!/bin/bash
cd /mnt/c/workspace/ewelink
export PATH="$HOME/.cargo/bin:$PATH"
set -a
. .env
set +a
BIN=./target/release/ewelink-rs
ID=a1b2c3d4e5
BASE=$(date +%s%3N)
echo "每次动作 = 打开 -> 保持5s -> 关闭；动作间隔 8s"
echo "round=# on_ms=# off_ms=# action_total_ms=# on=# off=#"
for i in 1 2 3 4 5; do
  ROUND_START=$((BASE + (i-1)*8000))
  NOW=$(date +%s%3N)
  if [ $NOW -lt $ROUND_START ]; then
    DIFF=$((ROUND_START - NOW))
    sleep $((DIFF/1000)).$(printf "%03d" $((DIFF%1000)))
  fi
  T0=$(date +%s%3N)                 # 动作开始：打开
  ON_RES=$(timeout 10 $BIN lan on $ID 2>&1 | tail -1)
  T1=$(date +%s%3N)
  ON_MS=$((T1-T0))
  sleep 5                           # 保持开启 5 秒
  T2=$(date +%s%3N)
  OFF_RES=$(timeout 10 $BIN lan off $ID 2>&1 | tail -1)
  T3=$(date +%s%3N)                 # 动作结束：关闭
  OFF_MS=$((T3-T2))
  ACTION=$((T3 - T0))
  echo "round=$i on_ms=$ON_MS off_ms=$OFF_MS action_total_ms=$ACTION on=$ON_RES off=$OFF_RES"
done
echo "--- final status ---"
timeout 15 $BIN lan status $ID 2>&1 | grep -A7 '"switches"' | head -9
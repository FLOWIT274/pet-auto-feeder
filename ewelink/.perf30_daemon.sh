#!/bin/bash
cd /mnt/c/workspace/ewelink
export PATH="$HOME/.cargo/bin:$PATH"
set -a
. .env
set +a
BIN=./target/release/ewelink-rs
ID=a1b2c3d4e5
# 确认 daemon 在跑（不在则自动拉起）
$BIN lan status $ID > /dev/null 2>&1
sleep 1
BASE=$(date +%s%3N)
echo "action=on->30s->off via daemon  interval=40s  rounds=5"
echo "round=# on_ms=# off_ms=# action_ms=# on=# off=#"
for i in 1 2 3 4 5; do
  ROUND_START=$((BASE + (i-1)*40000))
  NOW=$(date +%s%3N)
  if [ $NOW -lt $ROUND_START ]; then
    DIFF=$((ROUND_START - NOW))
    sleep $((DIFF/1000)).$(printf "%03d" $((DIFF%1000)))
  fi
  T0=$(date +%s%3N)
  ON_RES=$(timeout 15 $BIN lan on $ID 2>&1 | tail -1)
  T1=$(date +%s%3N)
  sleep 30
  T2=$(date +%s%3N)
  OFF_RES=$(timeout 15 $BIN lan off $ID 2>&1 | tail -1)
  T3=$(date +%s%3N)
  echo "round=$i on_ms=$((T1-T0)) off_ms=$((T3-T2)) action_ms=$((T3-T0)) on=$ON_RES off=$OFF_RES"
done
echo "--- final status ---"
timeout 15 $BIN lan status $ID 2>&1 | grep -A7 '"switches"' | head -9
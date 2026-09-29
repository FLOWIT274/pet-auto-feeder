#!/bin/sh
for m in soph_base soph_sys soph_jpeg soph_vcodec; do
  short=${m#soph_}
  [ "$short" = "vcodec" ] && continue
  awk -v mod="$m" -v out="$short" '$4=="["mod"]" && $2 ~ /^[TBDWRV]$/ {printf "0x00000000\t%s\t%s\tEXPORT_SYMBOL\t\n", $3, out}' /proc/kallsyms
done

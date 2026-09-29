#!/bin/sh
# wificfgd 宿主测试套件：编译并运行全部切片测试
set -e
cd "$(dirname "$0")/.."
OUT="${TMPDIR:-/tmp}/wificfgd-tests"
mkdir -p "$OUT"

CC=${CC:-gcc}
CFLAGS="-Wall -Wextra -I src"
# 测试用 TMPDIR_STR 宏定位临时文件（随 $OUT 变化）
TMPDEF="-DTMPDIR_STR=\"$OUT\""

$CC $CFLAGS tests/test_parse.c src/parse.c -o "$OUT"/test_parse
$CC $CFLAGS $TMPDEF tests/test_write.c src/write.c -o "$OUT"/test_write
$CC $CFLAGS $TMPDEF tests/test_http_get.c src/http.c src/parse.c src/write.c -o "$OUT"/test_http_get -lpthread
$CC $CFLAGS $TMPDEF tests/test_http_post.c src/http.c src/parse.c src/write.c -o "$OUT"/test_http_post -lpthread

fail=0
for t in test_parse test_write test_http_get test_http_post; do
    if "$OUT"/$t; then :; else fail=1; fi
done
for t in test_fallback.sh test_s30wifi.sh; do
    if sh tests/$t; then :; else fail=1; fi
done
echo "=== suite: $([ $fail -eq 0 ] && echo ALL GREEN || echo FAILURES) ==="
exit $fail
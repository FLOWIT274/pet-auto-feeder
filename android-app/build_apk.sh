#!/bin/bash
# build_apk.sh — 手工构建 licheerv-admin APK（无 gradle）
set -euo pipefail
SDK=/home/sbh/android-sdk
BT=$SDK/build-tools/33.0.2
AJ=$SDK/platforms/android-33/android.jar
ROOT="$(cd "$(dirname "$0")" && pwd)"
APP=$ROOT/app
OUT=$ROOT/build
KS=$ROOT/debug.keystore

rm -rf "$OUT"/classes "$OUT"/dex "$OUT"/compiled.zip "$OUT"/base.apk "$OUT"/aligned.apk
mkdir -p "$OUT"/classes "$OUT"/dex

echo "[1/5] aapt2 compile+link"
"$BT/aapt2" compile --dir "$APP/res" -o "$OUT/compiled.zip"
"$BT/aapt2" link -o "$OUT/base.apk" -I "$AJ" --manifest "$APP/AndroidManifest.xml" \
    --java "$OUT" --auto-add-overlay "$OUT/compiled.zip"

echo "[2/5] javac"
javac -source 8 -target 8 -bootclasspath "$AJ:$BT/core-lambda-stubs.jar" -classpath "$OUT" \
    -d "$OUT/classes" \
    "$APP/src/com/licheerv/admin/"*.java "$OUT/com/licheerv/admin/R.java"

echo "[3/5] d8"
"$BT/d8" --release --lib "$AJ" --output "$OUT/dex" \
    $(find "$OUT/classes" -name "*.class")

echo "[4/5] 打包 dex + zipalign"
cd "$OUT/dex" && zip -q -j "$OUT/base.apk" classes.dex && cd "$ROOT"
"$BT/zipalign" -f 4 "$OUT/base.apk" "$OUT/aligned.apk"

echo "[5/5] apksigner 签名"
if [ ! -f "$KS" ]; then
    keytool -genkeypair -v -keystore "$KS" -alias androiddebugkey \
        -storepass android -keypass android -keyalg RSA -validity 10000 \
        -dname "CN=Android Debug,O=Android,C=US" >/dev/null 2>&1
fi
"$BT/apksigner" sign --ks "$KS" --ks-pass pass:android --key-pass pass:android \
    --out "$ROOT/licheerv-admin.apk" "$OUT/aligned.apk"
"$BT/apksigner" verify "$ROOT/licheerv-admin.apk" && echo "APK OK: $ROOT/licheerv-admin.apk"
ls -la "$ROOT/licheerv-admin.apk"

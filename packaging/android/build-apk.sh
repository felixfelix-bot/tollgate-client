#!/bin/sh
# Build a signed debug APK without Gradle.
#
# Gradle+AGP is a 1 GB download with its own JDK opinions; this needs aapt2,
# javac, d8 and apksigner, all of which ship in the SDK we already installed.
# For one activity and one JNI library that is the whole build system.
set -eu

REPO=$(cd "$(dirname "$0")/../.." && pwd)
ANDROID=${ANDROID_SDK_ROOT:-$HOME/android-sdk}
BUILD_TOOLS=$(ls -d "$ANDROID"/build-tools/* | sort -V | tail -1)
PLATFORM=$(ls -d "$ANDROID"/platforms/android-* | sort -V | tail -1)
NDK=$(ls -d "$ANDROID"/ndk/* | sort -V | tail -1)
TC=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
API=26

# The box ships a JRE, not a JDK; prefer an explicit JAVA_HOME, then a local one.
if [ -z "${JAVA_HOME:-}" ] && [ -x "$HOME/.local/jdk/bin/javac" ]; then
    JAVA_HOME=$HOME/.local/jdk
fi
[ -n "${JAVA_HOME:-}" ] && PATH=$JAVA_HOME/bin:$PATH
command -v javac >/dev/null 2>&1 || { echo "no javac: set JAVA_HOME to a JDK 17+" >&2; exit 1; }

OUT=$REPO/target/android
rm -rf "$OUT"; mkdir -p "$OUT/gen" "$OUT/classes" "$OUT/dex" "$OUT/lib/arm64-v8a" "$OUT/lib/x86_64"

echo "== 1/6 native libraries (same core, two ABIs)"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$TC/aarch64-linux-android$API-clang
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER=$TC/x86_64-linux-android$API-clang
cd "$REPO"
cargo build --release --no-default-features --features android --target aarch64-linux-android
cargo build --release --no-default-features --features android --target x86_64-linux-android
cp target/aarch64-linux-android/release/libtollgate_client_core.so "$OUT/lib/arm64-v8a/"
cp target/x86_64-linux-android/release/libtollgate_client_core.so "$OUT/lib/x86_64/"

echo "== 2/6 resources"
"$BUILD_TOOLS/aapt2" compile --dir android/res -o "$OUT/res.zip"
"$BUILD_TOOLS/aapt2" link -o "$OUT/base.apk"     -I "$PLATFORM/android.jar"     --manifest android/AndroidManifest.xml     "$OUT/res.zip"     --java "$OUT/gen"     --min-sdk-version $API --target-sdk-version 34

echo "== 3/6 java"
javac -nowarn -source 17 -target 17 -classpath "$PLATFORM/android.jar"     -d "$OUT/classes" $(find android/java "$OUT/gen" -name '*.java')

echo "== 4/6 dex"
"$BUILD_TOOLS/d8" --lib "$PLATFORM/android.jar" --min-api $API --output "$OUT/dex"     $(find "$OUT/classes" -name '*.class')

echo "== 5/6 package"
cp "$OUT/base.apk" "$OUT/unsigned.apk"
cd "$OUT"
cp dex/classes.dex .
zip -q -r unsigned.apk classes.dex lib
"$BUILD_TOOLS/zipalign" -f 4 unsigned.apk aligned.apk

echo "== 6/6 sign"
KEYSTORE=${TOLLGATE_KEYSTORE:-$HOME/.android/tollgate-debug.keystore}
if [ ! -f "$KEYSTORE" ]; then
    mkdir -p "$(dirname "$KEYSTORE")"
    keytool -genkeypair -keystore "$KEYSTORE" -alias tollgate -storepass android         -keypass android -keyalg RSA -keysize 2048 -validity 10000         -dname "CN=TollGate Debug,O=TollGate,C=DE" >/dev/null 2>&1
fi
"$BUILD_TOOLS/apksigner" sign --ks "$KEYSTORE" --ks-pass pass:android     --key-pass pass:android --out "$REPO/tollgate-android.apk" aligned.apk

echo
echo "built: $REPO/tollgate-android.apk"
"$BUILD_TOOLS/aapt2" dump badging "$REPO/tollgate-android.apk" | head -3
"$BUILD_TOOLS/apksigner" verify --print-certs "$REPO/tollgate-android.apk" | head -3

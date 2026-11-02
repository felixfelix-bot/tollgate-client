#!/bin/sh
# Native feasibility builds only: neither feature authorizes Android payments.
set -eu
REPO=$(cd "$(dirname "$0")/../.." && pwd)
cd "$REPO"
ANDROID=${ANDROID_SDK_ROOT:-$HOME/android-sdk}
NDK=${ANDROID_NDK_HOME:-$ANDROID/ndk/26.1.10909125}
TC=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
FEATURE=${TOLLGATE_ANDROID_FEATURE:-android}
case "$FEATURE" in
    android|android-hardened) ;;
    *) printf '%s\n' 'unsupported Android feature (test harness forbidden)' >&2; exit 1 ;;
esac
# Resolve relative paths against the repo, consistently with build-apk.sh.
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$REPO/target}
case "$CARGO_TARGET_DIR" in /*) ;; *) CARGO_TARGET_DIR=$REPO/$CARGO_TARGET_DIR ;; esac
export CARGO_TARGET_DIR
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$TC/aarch64-linux-android26-clang
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER=$TC/x86_64-linux-android26-clang
export CC_aarch64_linux_android=$TC/aarch64-linux-android26-clang
export CC_x86_64_linux_android=$TC/x86_64-linux-android26-clang
export AR_aarch64_linux_android=$TC/llvm-ar
export AR_x86_64_linux_android=$TC/llvm-ar
for TARGET in aarch64-linux-android x86_64-linux-android; do
    cargo build --release --lib --locked --no-default-features --features "$FEATURE" --target "$TARGET"
done

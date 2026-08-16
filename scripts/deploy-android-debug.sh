#!/usr/bin/env bash
#
# Build the arm64 RustStation libretro core, and optionally get it onto a
# device.
#
#   scripts/deploy-android-debug.sh so         # build the .so, nothing else
#   scripts/deploy-android-debug.sh            # ... + copy to jniLibs + assembleDebug
#   scripts/deploy-android-debug.sh install    # ... + adb install -r
#
# **Use `so` while iterating.** It is a few seconds and it is all you need to
# know whether the core still builds for the device and what it weighs. The
# gradle step is minutes and only earns its keep when something is actually
# going to load the library.
#
# Why the rest of this exists: the core (RustStation) and the app
# (TrophyHubAndroid) are separate repos in the TrophyHub umbrella, so getting a
# core change onto a device is a fixed three-step dance. This pins it so no one
# rediscovers it. Only arm64-v8a ships (see the app's abiFilters); the devices
# are arm64.
#
# NOTE ON LOADING IT: the app resolves cores by bare filename through dlopen,
# which means the system linker finds them in the APK's own native library
# directory. **A pushed .so will not be picked up**; the APK has to be rebuilt
# and reinstalled. And until RustStation has a `CoreSlot` entry in
# TrophyHubAndroid, nothing in the app asks for this library at all, so `so`
# mode is the honest one to be running.
#
# Save states: the state format is versioned (psx_core::save::FORMAT_VERSION)
# and a core only accepts states at its own version. When a change bumps it, the
# desktop core has to be rebuilt in the same pass or the phone and the desktop
# stop agreeing.
set -euo pipefail

MODE="${1:-apk}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TRIPLE="aarch64-linux-android"
ABI="arm64-v8a"
SO="libpsxcore_libretro.so"
# RustStation is a standalone workspace, so its target dir is local, not the
# umbrella's shared TrophyHub/target. The umbrella's .cargo/config.toml still
# supplies the NDK linker + the 16 KB max-page-size link arg, because cargo
# walks up from the working directory to find it.
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"

echo "[1/4] cargo test --workspace"
( cd "$REPO" && cargo test --workspace )

echo "[2/4] cargo build --release -p psx-libretro --target $TRIPLE"
( cd "$REPO" && cargo build --release -p psx-libretro --target "$TRIPLE" )

SRC="$TARGET_DIR/$TRIPLE/release/$SO"
[ -f "$SRC" ] || { echo "ERROR: built core not found at $SRC" >&2; exit 1; }

# Keep a copy here in step with what was built, so "what is on the device" can
# be answered from this repo alone.
mkdir -p "$REPO/out/release"
OUT="$REPO/out/release/libpsxcore_libretro.android-arm64.so"
cp "$SRC" "$OUT"
echo "built: $OUT ($(du -h "$OUT" | cut -f1))"

if [ "$MODE" = "so" ]; then
    exit 0
fi

ANDROID="$(cd "$REPO/../TrophyHubAndroid" && pwd)"
DST="$ANDROID/app/src/main/jniLibs/$ABI/$SO"
echo "[3/4] cp core -> $DST"
mkdir -p "$(dirname "$DST")"
cp "$SRC" "$DST"

echo "[4/4] ./gradlew :app:assembleDebug"
( cd "$ANDROID" && ./gradlew :app:assembleDebug )

APK="$ANDROID/app/build/outputs/apk/debug/app-debug.apk"
echo "APK: $APK"

if [ "$MODE" = "install" ]; then
    # The primary test device shows up twice over wireless adb often enough
    # that a bare `adb install` picks the wrong entry or refuses outright.
    # Name the target explicitly: ANDROID_SERIAL if it is set, otherwise the
    # only device attached, and complain rather than guess if there are two.
    SERIAL="${ANDROID_SERIAL:-}"
    if [ -z "$SERIAL" ]; then
        mapfile -t DEVICES < <(adb devices | awk '$2 == "device" { print $1 }')
        case "${#DEVICES[@]}" in
            0) echo "ERROR: no authorized device attached" >&2; exit 1 ;;
            1) SERIAL="${DEVICES[0]}" ;;
            *) echo "ERROR: ${#DEVICES[@]} devices attached: ${DEVICES[*]}" >&2
               echo "       set ANDROID_SERIAL to choose one" >&2; exit 1 ;;
        esac
    fi
    echo "adb -s $SERIAL install -r"
    adb -s "$SERIAL" install -r "$APK"
fi

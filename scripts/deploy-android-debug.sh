#!/usr/bin/env bash
#
# One-liner: build the arm64 RustStation libretro core from the current working
# tree, drop it into the TrophyHubAndroid debug app's jniLibs, and assemble the
# debug APK (optionally install it to an attached device).
#
#   scripts/deploy-android-debug.sh            # build core -> jniLibs -> assembleDebug
#   scripts/deploy-android-debug.sh install    # ... then adb install -r the APK
#
# Why this exists: the core (RustStation) and the app (TrophyHubAndroid) are
# separate repos in the TrophyHub umbrella, so getting a core change onto a
# device is a fixed three-step dance. This pins it so no one rediscovers it.
# Only arm64-v8a ships (see the app's abiFilters); the two devices are arm64.
#
# NOTE: this core cannot run a game yet. There is no GPU, SPU, CD-ROM or
# controller, so on a device it boots a BIOS behind a black screen and that is
# all. The script exists now so the path is a script rather than a project when
# there is something to look at. Do not wire it into the app's core table until
# it draws something.
#
# Save states: the state format is versioned (psx_core::save::FORMAT_VERSION)
# and a core only accepts states at its own version. When a change bumps it, the
# desktop core has to be rebuilt in the same pass or the phone and the desktop
# stop agreeing.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ANDROID="$(cd "$REPO/../TrophyHubAndroid" && pwd)"
TRIPLE="aarch64-linux-android"
ABI="arm64-v8a"
SO="libpsxcore_libretro.so"
# RustStation is a standalone workspace, so its target dir is local, not the
# umbrella's shared TrophyHub/target. The umbrella's .cargo/config.toml still
# supplies the NDK linker + the 16 KB max-page-size link arg.
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"

echo "[0/3] cargo test --workspace"
( cd "$REPO" && cargo test --workspace )

echo "[1/3] cargo build --release -p psx-libretro --target $TRIPLE"
( cd "$REPO" && cargo build --release -p psx-libretro --target "$TRIPLE" )

SRC="$TARGET_DIR/$TRIPLE/release/$SO"
DST="$ANDROID/app/src/main/jniLibs/$ABI/$SO"
[ -f "$SRC" ] || { echo "ERROR: built core not found at $SRC" >&2; exit 1; }
echo "[2/3] cp core -> $DST"
mkdir -p "$(dirname "$DST")"
cp "$SRC" "$DST"
# Keep the copy under out/release in step with what shipped, so "what is on the
# device" can be answered from this repo alone.
mkdir -p "$REPO/out/release"
cp "$SRC" "$REPO/out/release/libpsxcore_libretro.android-arm64.so"

echo "[3/3] ./gradlew :app:assembleDebug"
( cd "$ANDROID" && ./gradlew :app:assembleDebug )

APK="$ANDROID/app/build/outputs/apk/debug/app-debug.apk"
echo "APK: $APK"

if [ "${1:-}" = "install" ]; then
  echo "adb install -r (device must be authorized for USB debugging)"
  adb install -r "$APK"
fi

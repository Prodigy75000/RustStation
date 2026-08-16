#!/usr/bin/env bash
# Run every disc in dumps/ for a fixed number of instructions and record what
# each one did: a screenshot, and the counters `shot` prints.
#
# This is a survey, not a test. It says how far each game gets today and gives
# the next session a list sorted by how close something is to working; it
# asserts nothing and cannot fail. The step count is deliberately the same for
# every disc so the results are comparable to each other rather than to a
# stopwatch.
#
#   scripts/survey.sh [steps]
set -u

STEPS="${1:-2000000000}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SHOT="$ROOT/target/release/shot.exe"
OUT="$ROOT/out/survey"
NTSC="$ROOT/bios/Sony PlayStation SCPH-1001 - DTLH-3000 BIOS v2.2 (1995-12-04)(Sony)(US).bin"
PAL="$ROOT/bios/Sony PlayStation SCPH-1002 BIOS v2.0 (1995-05-10)(Sony)(EU).bin"

mkdir -p "$OUT"
: > "$OUT/survey.txt"

find "$ROOT/dumps" -name '*.cue' | sort | while read -r cue; do
    name="$(basename "$cue" .cue)"
    # European releases carry SCES/SLES in their serial and need a PAL BIOS.
    # Nothing else here distinguishes them, so the file name is the only signal.
    case "$cue" in
        *SCES*|*SLES*|*-e-*) bios="$PAL"; region="PAL" ;;
        *) bios="$NTSC"; region="NTSC" ;;
    esac
    echo "== $name ($region)" | tee -a "$OUT/survey.txt"
    "$SHOT" "$bios" --disc "$cue" --steps "$STEPS" --hold start \
        --out "$OUT/$name.png" 2>&1 \
        | grep -E 'non-black|cdrom|gpu:|gte:|stubs|dma' \
        | tee -a "$OUT/survey.txt"
done

echo "survey written to $OUT/survey.txt"

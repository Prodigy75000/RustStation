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
#
# **Relative paths throughout, on purpose.** Under Git Bash on Windows the shell
# rewrites `/c/...` arguments into `C:\...` on the way to a native binary, and
# it declines to do that for arguments containing brackets. One disc here is
# called `Ace Combat 2 [SCES-00699]`, so with absolute paths that one disc, and
# only that one, was handed a path the binary could not open. Relative paths are
# never rewritten, so they cannot be rewritten inconsistently.
set -u

STEPS="${1:-2000000000}"
cd "$(dirname "$0")/.." || exit 1

SHOT="./target/release/shot.exe"
OUT="out/survey"
NTSC="bios/Sony PlayStation SCPH-1001 - DTLH-3000 BIOS v2.2 (1995-12-04)(Sony)(US).bin"
PAL="bios/Sony PlayStation SCPH-1002 BIOS v2.0 (1995-05-10)(Sony)(EU).bin"

mkdir -p "$OUT"
: > "$OUT/survey.txt"

find dumps -name '*.cue' | sort | while read -r cue; do
    name="$(basename "$cue" .cue)"
    # European releases carry SCES/SLES in their serial and need a PAL BIOS.
    # Nothing else here distinguishes them, so the file name is the only signal.
    case "$cue" in
        *SCES*|*SLES*|*-e-*) bios="$PAL"; region="PAL" ;;
        *) bios="$NTSC"; region="NTSC" ;;
    esac
    echo "== $name ($region)" | tee -a "$OUT/survey.txt"

    # Keep the whole output, then filter for display. Grepping the pipe
    # directly means a run that fails outright prints nothing at all and reads
    # as a disc that was skipped, which is how the one broken path above went
    # unnoticed through two full passes.
    if ! "$SHOT" "$bios" --disc "$cue" --steps "$STEPS" --hold start \
        --out "$OUT/$name.png" > "$OUT/$name.log" 2>&1
    then
        echo "   FAILED: $(head -1 "$OUT/$name.log")" | tee -a "$OUT/survey.txt"
        continue
    fi
    grep -E 'non-black|cdrom|gpu:|gte:|stubs|dma|unmapped|mdec' "$OUT/$name.log" \
        | tee -a "$OUT/survey.txt"
done

echo "survey written to $OUT/survey.txt"

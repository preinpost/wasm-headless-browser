#!/usr/bin/env bash
# Rebuilds crates/engine/fonts/NotoSansKR-subset.otf, the one font the engine
# measures and paints text with.
#
#   ./scripts/make-font.sh
#
# Noto Sans KR Regular, from the Noto CJK project (SIL Open Font License,
# crates/engine/fonts/OFL.txt), cut down to what Korean and Latin pages use:
# ASCII and Latin-1, common punctuation and symbols, Hangul compatibility
# jamo, and all 11,172 Hangul syllables, so that rare syllables in names
# paint too. Hanja are left out; a character the font lacks paints as an
# empty box rather than being guessed at. Hinting, layout and vertical
# tables go: the engine neither hints, shapes nor sets text vertically.
# Needs fontTools (pip install fonttools).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/crates/engine/fonts"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

curl -sSL -o "$WORK/full.otf" \
  https://github.com/notofonts/noto-cjk/raw/main/Sans/SubsetOTF/KR/NotoSansKR-Regular.otf
curl -sSL -o "$OUT/OFL.txt" https://github.com/notofonts/noto-cjk/raw/main/Sans/LICENSE

python3 - "$WORK/unicodes.txt" <<'PY'
import sys
points = set(range(0x20, 0x7F)) | set(range(0xA0, 0x100))
for lo, hi in [(0x2010, 0x2027), (0x2030, 0x203B), (0x2190, 0x2193), (0x2460, 0x2473),
               (0x25A0, 0x25CF), (0x2605, 0x2606), (0x3000, 0x3020), (0x3131, 0x318E),
               (0xFF01, 0xFF5E)]:
    points |= set(range(lo, hi + 1))
points |= {0x20A9, 0x2022, 0x203B, 0x2026, 0x2713, 0x2715, 0x00B7}
points |= set(range(0xAC00, 0xD7A4))
open(sys.argv[1], 'w').write(','.join(f'U+{p:04X}' for p in sorted(points)))
PY

pyftsubset "$WORK/full.otf" --unicodes-file="$WORK/unicodes.txt" \
  --layout-features='' --no-hinting --name-IDs='1,2,3,4,6' \
  --drop-tables+=BASE,VORG,vhea,vmtx,GSUB,GPOS \
  --output-file="$OUT/NotoSansKR-subset.otf"
ls -l "$OUT/NotoSansKR-subset.otf"

#!/bin/bash
# Шрифты и спрайты Protomaps basemaps-assets для оффлайн MapLibre.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MAP_DIR="$ROOT/data/map"
FONTS_BASE="https://protomaps.github.io/basemaps-assets/fonts"
SPRITE_BASE="https://protomaps.github.io/basemaps-assets/sprites/v4"

mkdir -p "$MAP_DIR/glyphs" "$MAP_DIR/sprites/v4"

fetch() {
  local dest="$1"
  local url="$2"
  mkdir -p "$(dirname "$dest")"
  if [ -f "$dest" ]; then
    return 0
  fi
  curl -fsSL --max-time 120 "$url" -o "$dest" || return 1
}

echo "==> glyphs (Noto Sans Regular)"
FONT="Noto%20Sans%20Regular"
for RANGE in 0-255 256-511 512-767 768-1023 1024-1279 1280-1535 1536-1791 1792-2047 2048-2303 2304-2559; do
  DEST="$MAP_DIR/glyphs/Noto Sans Regular/$RANGE.pbf"
  fetch "$DEST" "$FONTS_BASE/$FONT/$RANGE.pbf" || echo "WARN: glyph $RANGE" >&2
done

echo "==> sprites v4/light"
for FILE in light.json light.png light@2x.json light@2x.png; do
  fetch "$MAP_DIR/sprites/v4/$FILE" "$SPRITE_BASE/$FILE" || echo "WARN: sprite $FILE" >&2
done

echo "==> done: $(find "$MAP_DIR/glyphs" -name '*.pbf' | wc -l | tr -d ' ') glyph files"

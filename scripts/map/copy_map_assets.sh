#!/bin/bash
# Копирует maplibre-gl.css; генерирует style.json; проверяет pmtiles.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MAP_DIR="$ROOT/data/map"
UI="$ROOT/web-ui"

mkdir -p "$MAP_DIR"

echo "==> maplibre-gl.css"
if [ ! -f "$UI/node_modules/maplibre-gl/dist/maplibre-gl.css" ]; then
  (cd "$UI" && npm ci)
fi
cp "$UI/node_modules/maplibre-gl/dist/maplibre-gl.css" "$MAP_DIR/maplibre-gl.css"

echo "==> glyphs + sprites"
"$ROOT/scripts/map/download_map_assets.sh"

echo "==> style.json"
if [ ! -f "$UI/node_modules/@protomaps/basemaps/package.json" ]; then
  (cd "$UI" && npm install @protomaps/basemaps --save-dev)
fi
node "$ROOT/scripts/map/generate_style.mjs"

if [ -f "$MAP_DIR/ru_cis.pmtiles" ]; then
  echo "OK: ru_cis.pmtiles ($(wc -c <"$MAP_DIR/ru_cis.pmtiles") bytes)"
else
  echo "WARN: data/map/ru_cis.pmtiles отсутствует — см. data/map/README.md"
fi

echo "Готово: $MAP_DIR"

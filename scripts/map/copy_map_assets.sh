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

if [ -f "$ROOT/scripts/map/fetch_supermap_rworgs.py" ]; then
  echo "==> railways_zones.geojson (Supermap WFS, нужен интернет)"
  if command -v uv >/dev/null 2>&1 && [ -f "$ROOT/scripts/map/pyproject.toml" ]; then
    (cd "$ROOT/scripts/map" && uv run python fetch_supermap_rworgs.py) \
      || echo "WARN: fetch_supermap_rworgs failed (см. data/map/README.md)"
  else
    echo "SKIP: нужен uv; ./scripts/map/run.sh fetch-zones"
  fi
fi
if [ -f "$MAP_DIR/railways_zones.geojson" ]; then
  echo "OK: railways_zones.geojson"
else
  echo "WARN: data/map/railways_zones.geojson отсутствует — git pull или run.sh fetch-zones"
fi

echo "Готово: $MAP_DIR"

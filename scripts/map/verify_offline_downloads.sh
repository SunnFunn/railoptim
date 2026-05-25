#!/bin/bash
# Этап 1 плана оффлайн-подложки: проверка доступности CDN и локальных артефактов.
# PMTiles RU+СНГ: скачивается вручную с build.protomaps.com или через PMtiles_URL.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MAP_DIR="$ROOT/data/map"
REPORT="$MAP_DIR/verify_report.txt"
MANIFEST="$MAP_DIR/download_manifest.json"
PMTILES_PATH="$MAP_DIR/ru_cis.pmtiles"

mkdir -p "$MAP_DIR"
: >"$REPORT"

log() {
  echo "$1" | tee -a "$REPORT"
}

log "=== railoptim: verify offline map downloads ==="
log "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
log ""

# --- 1.1 maplibre-gl.css из node_modules ---
log "=== 1.1 maplibre-gl.css (local npm) ==="
UI="$ROOT/web-ui"
if [ ! -d "$UI/node_modules/maplibre-gl" ]; then
  if command -v npm >/dev/null 2>&1; then
    log "npm ci in web-ui..."
    (cd "$UI" && npm ci) >>"$REPORT" 2>&1 || true
  else
    log "FAIL: npm не найден, node_modules/maplibre-gl отсутствует"
  fi
fi
CSS_SRC="$UI/node_modules/maplibre-gl/dist/maplibre-gl.css"
if [ -f "$CSS_SRC" ]; then
  cp "$CSS_SRC" "$MAP_DIR/maplibre-gl.css"
  log "OK: скопирован $MAP_DIR/maplibre-gl.css ($(wc -c <"$MAP_DIR/maplibre-gl.css") bytes)"
else
  log "FAIL: $CSS_SRC не найден"
fi
log ""

# --- 1.2 CDN probe ---
log "=== 1.2 CDN probe (диагностика блокировки) ==="
probe() {
  local url="$1"
  if curl -fsSI --max-time 15 "$url" 2>>"$REPORT" | head -1 | tee -a "$REPORT"; then
    log "  reachable: $url"
  else
    log "  BLOCKED/FAIL: $url"
  fi
}
probe "https://unpkg.com/maplibre-gl@5/dist/maplibre-gl.css"
probe "https://tiles.openfreemap.org/styles/liberty"
log ""

# --- 1.3 OpenFreeMap dependency audit ---
log "=== 1.3 OpenFreeMap style audit ==="
LIBERTY="/tmp/railoptim_liberty.json"
if curl -fsS --max-time 30 "https://tiles.openfreemap.org/styles/liberty" -o "$LIBERTY" 2>>"$REPORT"; then
  log "OK: liberty.json скачан"
  if command -v python3 >/dev/null 2>&1; then
    python3 - <<'PY' >>"$REPORT" 2>&1
import json, re, sys
from urllib.parse import urlparse
with open("/tmp/railoptim_liberty.json") as f:
    d = json.load(f)
urls = set()
def walk(o):
    if isinstance(o, dict):
        for v in o.values(): walk(v)
    elif isinstance(o, list):
        for v in o: walk(v)
    elif isinstance(o, str) and re.match(r"https?://", o):
        urls.add(o.split("{")[0].rstrip("/") if "{" in o else o)
walk(d)
for u in sorted(urls)[:30]:
    print("  url:", u)
print(f"  (всего уникальных URL-префиксов: {len(urls)})")
PY
    log "  вывод: десятки внешних хостов — полное зеркало OpenFreeMap непрактично"
  fi
else
  log "SKIP/FAIL: не удалось скачать liberty.json (ожидаемо при блокировке)"
fi
log ""

# --- 1.4 PMTiles ---
log "=== 1.4 PMTiles (ru_cis.pmtiles) ==="
if [ -f "$PMTILES_PATH" ]; then
  SZ=$(wc -c <"$PMTILES_PATH")
  log "OK: уже есть $PMTILES_PATH ($SZ bytes)"
  file "$PMTILES_PATH" >>"$REPORT" 2>&1 || true
elif [ -n "${PMTILES_URL:-}" ]; then
  log "Скачивание PMtiles_URL -> $PMTILES_PATH"
  curl -fL --retry 3 --progress-bar -o "$PMTILES_PATH" "$PMTILES_URL"
  log "OK: скачан $(wc -c <"$PMTILES_PATH") bytes"
else
  log "MISSING: $PMTILES_PATH"
  log "  Скачайте RU+СНГ с https://build.protomaps.com (max zoom 13–14)"
  log "  Затем: curl -L -o $PMTILES_PATH '<URL>'"
  log "  Или: PMtiles_URL='https://...' $0"
  if [ "${DOWNLOAD_SAMPLE:-0}" = "1" ]; then
    SAMPLE_URL="${PMTILES_SAMPLE_URL:-https://pmtiles.io/protomaps(vector)ODbL_fadi.pmtiles}"
    log "  DOWNLOAD_SAMPLE=1: качаем тестовый Monaco (~3MB)..."
    if curl -fL --retry 3 -o "$PMTILES_PATH" "$SAMPLE_URL" 2>>"$REPORT"; then
      log "  OK: sample monaco для smoke (не RU+СНГ!)"
    else
      log "  FAIL: sample download"
    fi
  fi
fi
log ""

# --- 1.5 glyphs/sprites ---
log "=== 1.5 glyphs / sprites (basemaps-assets) ==="
if [ -x "$ROOT/scripts/map/download_map_assets.sh" ]; then
  "$ROOT/scripts/map/download_map_assets.sh" 2>&1 | tee -a "$REPORT" || log "WARN: download_map_assets частично failed"
else
  log "SKIP: scripts/map/download_map_assets.sh не найден"
fi
GLYPH_COUNT=$(find "$MAP_DIR/glyphs" -name '*.pbf' 2>/dev/null | wc -l | tr -d ' ')
log "glyph .pbf files: $GLYPH_COUNT"
log ""

# --- manifest ---
GO="no-go"
HAS_CSS=false
HAS_STYLE=false
HAS_SPRITE=false
HAS_PMTILES=false

[ -f "$MAP_DIR/maplibre-gl.css" ] && HAS_CSS=true
[ -f "$MAP_DIR/style.json" ] && HAS_STYLE=true
[ -f "$MAP_DIR/sprites/v4/light.json" ] && HAS_SPRITE=true
[ -f "$PMTILES_PATH" ] && HAS_PMTILES=true

if $HAS_CSS && $HAS_STYLE && $HAS_SPRITE; then
  if $HAS_PMTILES; then
    SZ=$(wc -c <"$PMTILES_PATH")
    if [ "$SZ" -gt 50000000 ]; then
      GO="go"
    elif [ "$SZ" -gt 100000 ]; then
      GO="go-smoke"
      log "WARN: pmtiles < 50MB — для prod нужен полный RU+СНГ с build.protomaps.com"
    else
      GO="go-smoke"
      log "WARN: очень маленький pmtiles ($SZ bytes)"
    fi
  else
    GO="go-assets"
    log "WARN: ru_cis.pmtiles отсутствует — статика готова, тайлы добавьте вручную"
  fi
fi

cat >"$MANIFEST" <<EOF
{
  "verified_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "decision": "$GO",
  "pmtiles_path": "data/map/ru_cis.pmtiles",
  "pmtiles_bytes": $( [ -f "$PMTILES_PATH" ] && wc -c <"$PMTILES_PATH" || echo 0 ),
  "maplibre_css": $HAS_CSS,
  "style_json": $HAS_STYLE,
  "sprite_json": $HAS_SPRITE,
  "glyph_pbf_count": $GLYPH_COUNT
}
EOF

log "=== ИТОГ: $GO ==="
log "manifest: $MANIFEST"
log "report: $REPORT"
if [ "$GO" = "no-go" ]; then
  log ""
  log "Нужны: maplibre-gl.css, style.json, sprites/v4/light.json"
  log "Для полной карты: ru_cis.pmtiles в data/map/"
  exit 1
fi
if [ "$GO" = "go-assets" ]; then
  log ""
  log "Статика OK. Добавьте ru_cis.pmtiles перед prod."
  exit 0
fi
exit 0

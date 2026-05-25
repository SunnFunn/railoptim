#!/bin/bash
# Собирает data/map/ru_cis.pmtiles из дневного planet build Protomaps (HTTP Range, без скачивания ~120 GB).
# Файл в .gitignore — на prod: rsync / Google Drive (см. data/map/README.md).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MAP_DIR="$ROOT/data/map"
OUT="$MAP_DIR/ru_cis.pmtiles"
PMTILES_BIN="${PMTILES_BIN:-$ROOT/.tools/pmtiles}"

# Дневной build: https://maps.protomaps.com/builds/
PLANET_URL="${PMTILES_PLANET_URL:-https://build.protomaps.com/20260521.pmtiles}"

# RU + СНГ + Прибалтика: Kaliningrad → Chukotka, Кавказ/Ср. Азия → север РФ
BBOX="${PMTILES_BBOX:-19,35,180,82}"
MAXZOOM="${PMTILES_MAXZOOM:-13}"

install_pmtiles() {
  if [ -x "$PMTILES_BIN" ]; then
    return
  fi
  mkdir -p "$ROOT/.tools"
  local arch zip url
  arch="$(uname -m)"
  case "$(uname -s)" in
    Darwin)
      if [ "$arch" = "arm64" ]; then
        zip="go-pmtiles-1.30.2_Darwin_arm64.zip"
      else
        zip="go-pmtiles-1.30.2_Darwin_x86_64.zip"
      fi
      ;;
    Linux)
      if [ "$arch" = "aarch64" ] || [ "$arch" = "arm64" ]; then
        zip="go-pmtiles_1.30.2_Linux_arm64.tar.gz"
      else
        zip="go-pmtiles_1.30.2_Linux_x86_64.tar.gz"
      fi
      ;;
    *)
      echo "Unsupported OS; установите pmtiles в PATH или задайте PMTILES_BIN" >&2
      exit 1
      ;;
  esac
  url="https://github.com/protomaps/go-pmtiles/releases/download/v1.30.2/$zip"
  echo "Installing pmtiles from $url ..."
  curl -fsSL -o "$ROOT/.tools/pmtiles-dl" "$url"
  if [[ "$zip" == *.zip ]]; then
    unzip -o -j "$ROOT/.tools/pmtiles-dl" -d "$ROOT/.tools"
  else
    tar -xzf "$ROOT/.tools/pmtiles-dl" -C "$ROOT/.tools"
  fi
  chmod +x "$PMTILES_BIN"
}

mkdir -p "$MAP_DIR"
install_pmtiles

if [ "${1:-}" = "--dry-run" ]; then
  exec "$PMTILES_BIN" extract "$PLANET_URL" "$OUT" --bbox="$BBOX" --maxzoom="$MAXZOOM" --dry-run
fi

TMP="${OUT}.partial"
THREADS="${PMTILES_THREADS:-2}"
MAX_ATTEMPTS="${PMTILES_MAX_ATTEMPTS:-5}"

echo "Source: $PLANET_URL"
echo "Output: $OUT (bbox=$BBOX maxzoom=$MAXZOOM threads=$THREADS)"

attempt=1
while [ "$attempt" -le "$MAX_ATTEMPTS" ]; do
  echo "Attempt $attempt/$MAX_ATTEMPTS ..."
  rm -f "$TMP" "$OUT"
  if "$PMTILES_BIN" extract "$PLANET_URL" "$TMP" --bbox="$BBOX" --maxzoom="$MAXZOOM" \
    --download-threads="$THREADS" -q \
    && "$PMTILES_BIN" show "$TMP" >/dev/null; then
    mv "$TMP" "$OUT"
    echo "Done: $(wc -c <"$OUT") bytes -> $OUT"
    "$PMTILES_BIN" show "$OUT" | head -8
    exit 0
  fi
  echo "WARN: extract failed or archive invalid, retrying..." >&2
  attempt=$((attempt + 1))
  sleep 5
done

echo "FAIL: не удалось собрать $OUT за $MAX_ATTEMPTS попыток" >&2
exit 1

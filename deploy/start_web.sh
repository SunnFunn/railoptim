#!/bin/bash
# Wrapper для systemd: запуск prod-бинарника из app/bin/.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN="$ROOT/app/bin/railoptim-web"
if [ ! -x "$BIN" ]; then
    echo "railoptim-web: бинарник не найден: $BIN" >&2
    echo "  выполните: ./deploy/install_web_service.sh" >&2
    exit 1
fi

export WEB_BIND_ADDR="${WEB_BIND_ADDR:-0.0.0.0:8080}"
export STATIONS_GEO_DB="${STATIONS_GEO_DB:-$ROOT/data/stations/stations_geo.sqlite}"
export OPTIM_RESULT_DIR="${OPTIM_RESULT_DIR:-$ROOT/tmp}"
export WEB_STATIC_DIR="${WEB_STATIC_DIR:-$ROOT/web-ui/dist}"
export WEB_CORS_ORIGINS="${WEB_CORS_ORIGINS:-*}"
export RUST_LOG="${RUST_LOG:-railoptim_web=info,tower_http=info,axum=info}"

exec "$BIN"

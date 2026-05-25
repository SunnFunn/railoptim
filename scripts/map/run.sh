#!/bin/bash
# Python-скрипты карты через uv (без python -m venv).
#
#   ./run.sh sync              — uv sync (зависимости в scripts/map/.venv)
#   ./run.sh sync --offline    — только из локального кэша uv (оффлайн prod)
#   ./run.sh build-voronoi     — railways_voronoi.geojson
#   ./run.sh python -c '...'   — произвольная команда в окружении проекта

set -euo pipefail

MAP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$MAP_DIR"

if ! command -v uv >/dev/null 2>&1; then
    echo "uv не найден в PATH. Установите: https://docs.astral.sh/uv/" >&2
    exit 1
fi

usage() {
    cat <<'EOF'
Usage:
  run.sh sync [--frozen] [--offline]   — установить зависимости (uv.lock)
  run.sh build-voronoi [args…]         — build_railway_voronoi.py
  run.sh [args…]                       — то же, что build-voronoi по умолчанию

Примеры:
  ./scripts/map/run.sh sync
  ./scripts/map/run.sh build-voronoi --region ru,cis
  cd scripts/map && uv sync --frozen --offline && ./run.sh
EOF
}

cmd="${1:-build-voronoi}"
case "$cmd" in
    -h|--help|help)
        usage
        exit 0
        ;;
    sync)
        shift
        exec uv sync "$@"
        ;;
    build-voronoi|voronoi)
        shift
        exec uv run python build_railway_voronoi.py "$@"
        ;;
    *)
        exec uv run "$@"
        ;;
esac

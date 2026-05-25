#!/bin/bash
# Python-скрипты карты через uv (без python -m venv).
#
#   ./run.sh fetch-zones        — Supermap WFS → railways_zones.geojson (рекомендуется)
#   ./run.sh build-voronoi      — устаревший Voronoi (не использовать для prod)
#   ./run.sh sync               — uv sync

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
  run.sh sync [--frozen] [--offline]     — uv sync
  run.sh fetch-zones [args…]               — Supermap WFS → data/map/railways_zones.geojson
  run.sh build-voronoi [args…]             — Voronoi (legacy)

Примеры:
  ./scripts/map/run.sh fetch-zones
  ./scripts/map/run.sh fetch-zones --skip-download   # из supermap_rworgs_raw.geojson
EOF
}

cmd="${1:-fetch-zones}"
case "$cmd" in
    -h|--help|help)
        usage
        exit 0
        ;;
    sync)
        shift
        exec uv sync "$@"
        ;;
    fetch-zones|zones|supermap)
        shift
        exec uv run python fetch_supermap_rworgs.py "$@"
        ;;
    build-voronoi|voronoi)
        shift
        exec uv run python build_railway_voronoi.py "$@"
        ;;
    *)
        exec uv run "$@"
        ;;
esac

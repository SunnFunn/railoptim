#!/bin/bash
# Geofabrik PBF — только внешняя сеть, без Infisical и без proxy-trap.
#
#   ./scripts/stations/download-pbf.sh [args для build_osm_esr_index.py…]
#
# Запускать на машине/окне, где разрешён исходящий HTTPS (не оффлайн-режим run.sh).

set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_infisical_env.sh
source "$dir/_infisical_env.sh"

cd "$(stations_repo_root)"

clear_proxy

BUILD_SCRIPT="$dir/build_osm_esr_index.py"
if [ ! -f "$BUILD_SCRIPT" ]; then
    echo "download-pbf: $BUILD_SCRIPT ещё не реализован (пункт 3 плана)." >&2
    echo "Скрипт будет: python3 build_osm_esr_index.py --download …" >&2
    echo "Манифест регионов: data/stations/geofabrik_regions.yaml" >&2
    exit 2
fi

python3 "$BUILD_SCRIPT" --download "$@"

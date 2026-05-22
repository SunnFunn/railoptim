#!/bin/bash
# Полный ETL справочника станций: NSI → OSM → stations_geo.sqlite
#
#   ./scripts/stations/build_all.sh [dev|prod] [options]
#
# Шаги (можно пропускать флагами):
#   1. fetch-nsi   — MSSQL + Infisical, proxy-trap
#   2. download-pbf + build-osm --index — интернет, без proxy
#   3. build-geo   — join → SQLite (оффлайн OK)
#
# Options:
#   --skip-nsi            не выгружать NSI
#   --skip-osm            не скачивать PBF и не строить osm_esr_index
#   --skip-geo            не собирать SQLite
#   --include-optional    china-latest и пр. optional регионы
#   --osm-args '…'        доп. аргументы для build_osm_esr_index.py (шаг 2b)
#
# Examples:
#   ./scripts/stations/build_all.sh prod
#   ./scripts/stations/build_all.sh prod --skip-nsi --skip-osm   # только join
#   ./scripts/stations/build_all.sh dev --include-optional

set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$dir/../.." && pwd)"
cd "$repo"

usage() {
    cat <<'EOF'
Usage: build_all.sh [dev|prod] [options]

Options:
  --skip-nsi
  --skip-osm
  --skip-geo
  --include-optional
  --osm-args 'ARGS'     передать в build_osm_esr_index.py (шаг index)
  -h, --help

Артефакты:
  data/stations/stations_nsi_raw.parquet
  data/stations/osm_esr_index.parquet
  data/stations/stations_geo.sqlite
  data/stations/build_report.json
EOF
}

ENV="dev"
SKIP_NSI=false
SKIP_OSM=false
SKIP_GEO=false
INCLUDE_OPTIONAL=false
OSM_EXTRA=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        dev|prod|staging)
            ENV="$1"
            shift
            ;;
        --skip-nsi)
            SKIP_NSI=true
            shift
            ;;
        --skip-osm)
            SKIP_OSM=true
            shift
            ;;
        --skip-geo)
            SKIP_GEO=true
            shift
            ;;
        --include-optional)
            INCLUDE_OPTIONAL=true
            shift
            ;;
        --osm-args)
            shift
            # shellcheck disable=SC2206
            OSM_EXTRA=($1)
            shift
            ;;
        -h|--help|help)
            usage
            exit 0
            ;;
        *)
            echo "build_all.sh: неизвестный аргумент: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

OSM_FLAGS=()
if $INCLUDE_OPTIONAL; then
    OSM_FLAGS+=("--include-optional")
fi

start=$(date +%s)
echo "[build_all] env=$ENV repo=$repo"

if ! $SKIP_NSI; then
    echo ""
    echo "========== 1/3 NSI (MSSQL) =========="
    "$dir/fetch-nsi.sh" "$ENV"
else
    echo "[build_all] skip NSI"
fi

if ! $SKIP_OSM; then
    echo ""
    echo "========== 2/3 OSM (Geofabrik) =========="
    set -- "${OSM_FLAGS[@]}"
    "$dir/download-pbf.sh" "$@"
    set -- --index "${OSM_FLAGS[@]}" "${OSM_EXTRA[@]+"${OSM_EXTRA[@]}"}"
    "$dir/build-osm.sh" "$@"
else
    echo "[build_all] skip OSM"
fi

if ! $SKIP_GEO; then
    echo ""
    echo "========== 3/3 GEO (SQLite) =========="
    "$dir/build-geo.sh"
else
    echo "[build_all] skip GEO"
fi

end=$(date +%s)
echo ""
echo "[build_all] готово за $((end - start)) сек."
echo "  NSI:    data/stations/stations_nsi_raw.parquet"
echo "  OSM:    data/stations/osm_esr_index.parquet"
echo "  SQLite: data/stations/stations_geo.sqlite"
echo "  Отчёт:  data/stations/build_report.json"
echo ""
echo "Проверка: ./scripts/stations/run.sh sample-geo --n 20"

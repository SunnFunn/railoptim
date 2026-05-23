#!/bin/bash
# ETL справочника станций ЕСР — диспетчер скриптов.
#
#   fetch-nsi   — MSSQL + Infisical, proxy-trap (оффлайн-машина)
#   download-pbf / build-osm / build-sbin — Geofabrik / sbin, без proxy-trap (интернет)
#   build-all   — полный pipeline NSI → OSM → SQLite
#   sample-geo  — выборка из SQLite
#   test        — unit-тесты без БД
#   sample-nsi  — случайная выборка из parquet для визуальной проверки

set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_python_env.sh
source "$dir/_python_env.sh"

usage() {
    cat <<'EOF'
Usage:
  run.sh [dev|prod] fetch-nsi [python args…]   — MSSQL → parquet (default)
  run.sh download-pbf [args…]   — только скачать PBF
  run.sh build-osm [args…]      — download + index (или --index из cache)
  run.sh build-all [dev|prod] [opts]  — полный ETL (см. build_all.sh --help)
  run.sh build-sbin [args…]      — Tier2: osm.sbin.ru → parquet
  run.sh build-geo [args…]        — NSI + OSM + sbin → SQLite
  run.sh sample-geo [--n 20]      — выборка stations_geo.sqlite
  run.sh sample-osm [--n 20]        — выборка osm_esr_index.parquet
  run.sh test
  run.sh sample-nsi [--n 30] [python args…]

Сеть:
  fetch-nsi       — proxy-trap; Infisical localhost:9000
  download-pbf / build-osm / build-sbin — proxy снят; Infisical не нужен

Examples:
  ./scripts/stations/run.sh
  ./scripts/stations/run.sh prod fetch-nsi
  ./scripts/stations/run.sh download-pbf
  ./scripts/stations/run.sh build-all prod
  ./scripts/stations/run.sh build-geo
  ./scripts/stations/run.sh sample-geo --n 25
  ./scripts/stations/run.sh sample-nsi --n 25
EOF
}

ENV=""
CMD="fetch-nsi"
FORWARD=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        dev|prod|staging)
            ENV="$1"
            shift
            ;;
        fetch-nsi|download-pbf|build-osm|build-sbin|build-geo|build-all|test|sample-nsi|sample-osm|sample-geo)
            CMD="$1"
            shift
            ;;
        -h|--help|help)
            usage
            exit 0
            ;;
        *)
            FORWARD+=("$1")
            shift
            ;;
    esac
done

case "$CMD" in
    fetch-nsi)
        if [ -n "$ENV" ]; then
            set -- "$ENV" "${FORWARD[@]+"${FORWARD[@]}"}"
        else
            set -- "${FORWARD[@]+"${FORWARD[@]}"}"
        fi
        exec "$dir/fetch-nsi.sh" "$@"
        ;;
    download-pbf)
        if [ -n "$ENV" ]; then
            echo "run.sh: env ($ENV) для download-pbf не используется" >&2
        fi
        set -- "${FORWARD[@]+"${FORWARD[@]}"}"
        exec "$dir/download-pbf.sh" "$@"
        ;;
    build-osm)
        if [ -n "$ENV" ]; then
            echo "run.sh: env ($ENV) для build-osm не используется" >&2
        fi
        set -- "${FORWARD[@]+"${FORWARD[@]}"}"
        exec "$dir/build-osm.sh" "$@"
        ;;
    build-sbin)
        if [ -n "$ENV" ]; then
            echo "run.sh: env ($ENV) для build-sbin не используется" >&2
        fi
        set -- "${FORWARD[@]+"${FORWARD[@]}"}"
        exec "$dir/build-sbin.sh" "$@"
        ;;
    test)
        if [ -n "$ENV" ] || [ "${#FORWARD[@]}" -gt 0 ]; then
            echo "run.sh test: не принимает env и доп. аргументы" >&2
            exit 1
        fi
        exec "$dir/tests/run_all.sh"
        ;;
    build-geo)
        if [ -n "$ENV" ]; then
            echo "run.sh: env ($ENV) для build-geo не используется" >&2
        fi
        set -- "${FORWARD[@]+"${FORWARD[@]}"}"
        exec "$dir/build-geo.sh" "$@"
        ;;
    build-all)
        if [ -n "$ENV" ]; then
            set -- "$ENV" "${FORWARD[@]+"${FORWARD[@]}"}"
        else
            set -- "${FORWARD[@]+"${FORWARD[@]}"}"
        fi
        exec "$dir/build_all.sh" "$@"
        ;;
    sample-nsi)
        if [ -n "$ENV" ]; then
            echo "run.sh: env ($ENV) для sample-nsi не используется" >&2
        fi
        set -- "${FORWARD[@]+"${FORWARD[@]}"}"
        run_stations_python "$dir/tools/sample_nsi_parquet.py" "$@"
        ;;
    sample-osm)
        if [ -n "$ENV" ]; then
            echo "run.sh: env ($ENV) для sample-osm не используется" >&2
        fi
        set -- "${FORWARD[@]+"${FORWARD[@]}"}"
        run_stations_python "$dir/tools/sample_osm_parquet.py" "$@"
        ;;
    sample-geo)
        if [ -n "$ENV" ]; then
            echo "run.sh: env ($ENV) для sample-geo не используется" >&2
        fi
        set -- "${FORWARD[@]+"${FORWARD[@]}"}"
        run_stations_python "$dir/tools/sample_stations_geo.py" "$@"
        ;;
    *)
        usage >&2
        exit 1
        ;;
esac

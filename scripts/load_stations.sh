#!/bin/bash
# Генерация data/load_stations.json: парсинг LoadStations.xlsx (Rust, calamine) +
# подбор кодов ЕСР-6 в MSSQL через src/data/load_stations_esr.py.
#
# Секреты MSSQL_* грузятся из self-hosted Infisical (как в run.sh и stations ETL):
# бинарник наследует окружение и передаёт его дочернему python3 (pymssql).
#
#   ./scripts/load_stations.sh [dev|prod] [--input <xlsx>] [--output <json>] [--dry-run]
#
#   dev  — бинарник из target/release (соберётся при отсутствии);
#   prod — бинарник из app/bin;
#   --dry-run — только парсинг Excel, без Infisical и MSSQL (load_station_code=null).

set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$dir/.." && pwd)"
# shellcheck source=stations/_infisical_env.sh
source "$dir/stations/_infisical_env.sh"

cd "$repo_root"

ENV="dev"
DRY_RUN=false
BIN_ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        dev|prod|staging)
            ENV="$1"
            shift
            ;;
        --dry-run)
            DRY_RUN=true
            BIN_ARGS+=("$1")
            shift
            ;;
        -h|--help|help)
            echo "Usage: load_stations.sh [dev|prod] [--input <xlsx>] [--output <json>] [--dry-run]"
            exit 0
            ;;
        *)
            BIN_ARGS+=("$1")
            shift
            ;;
    esac
done

# Выбор бинарника как в run.sh: prod из app/bin, dev из target/release.
if [ "$ENV" = "prod" ]; then
    BIN="$repo_root/app/bin/railoptim-load-stations"
else
    BIN="$repo_root/target/release/railoptim-load-stations"
    if [ ! -x "$BIN" ]; then
        echo "--- Сборка railoptim-load-stations (release) ---"
        cargo build --release --bin railoptim-load-stations
    fi
fi

if [ ! -x "$BIN" ]; then
    echo "Ошибка: не найден бинарник $BIN (соберите его или запустите install)." >&2
    exit 1
fi

# Парсинг без БД: секреты не нужны.
if [ "$DRY_RUN" = "true" ]; then
    echo "--- dry-run: без Infisical и MSSQL ---"
    exec "$BIN" "${BIN_ARGS[@]+"${BIN_ARGS[@]}"}"
fi

apply_offline_proxy
load_infisical_secrets "$ENV" true

# MSSQL — TCP; на случай HTTP-обёрток добавляем хост в no_proxy.
if [ -n "${MSSQL_SERVER_MSKASUVPL:-}" ]; then
    mssql_host="${MSSQL_SERVER_MSKASUVPL%%,*}"
    mssql_host="${mssql_host%%\\*}"
    extend_no_proxy "$mssql_host"
fi

exec "$BIN" "${BIN_ARGS[@]+"${BIN_ARGS[@]}"}"

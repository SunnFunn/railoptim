#!/bin/bash
# NSI.Station (MSSQL) → parquet. Оффлайн-режим: proxy-trap + локальный Infisical.
#
#   ./scripts/stations/fetch-nsi.sh [dev|prod] [args для bin/fetch_nsi.py…]

set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_infisical_env.sh
source "$dir/_infisical_env.sh"

cd "$(stations_repo_root)"

ENV="dev"
PY_ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        dev|prod|staging)
            ENV="$1"
            shift
            ;;
        -h|--help|help)
            echo "Usage: fetch-nsi.sh [dev|prod] [python args…]"
            exit 0
            ;;
        *)
            PY_ARGS+=("$1")
            shift
            ;;
    esac
done

apply_offline_proxy
load_infisical_secrets "$ENV" true

# MSSQL — TCP; на случай HTTP-обёрток добавляем хост в no_proxy.
if [ -n "${MSSQL_SERVER_MSKASUVPL:-}" ]; then
    mssql_host="${MSSQL_SERVER_MSKASUVPL%%,*}"
    mssql_host="${mssql_host%%\\*}"
    extend_no_proxy "$mssql_host"
fi

run_stations_python "$dir/bin/fetch_nsi.py" "${PY_ARGS[@]+"${PY_ARGS[@]}"}"

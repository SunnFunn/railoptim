# Общие функции для scripts/stations/*.sh (source, не запускать напрямую).

# shellcheck source=_python_env.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/_python_env.sh"

STATIONS_PROJECT_ID="a28f09d6-1840-4ac3-ad90-f8c9464facef"

stations_repo_root() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[1]}")" && pwd)"
    cd "$script_dir/../.." && pwd
}

# Как run.sh: HTTP-клиенты (infisical CLI) — только no_proxy; не ходить в интернет.
# pymssql использует TCP и proxy не читает.
apply_offline_proxy() {
    export http_proxy="http://127.0.0.1:1"
    export https_proxy="http://127.0.0.1:1"
    export all_proxy="http://127.0.0.1:1"
    export HTTP_PROXY="$http_proxy"
    export HTTPS_PROXY="$https_proxy"
    export ALL_PROXY="$all_proxy"
    export no_proxy="${STATIONS_NO_PROXY:-localhost,127.0.0.1,0.0.0.0,10.10.100.238,10.10.101.183,10.10.101.78,10.10.100.47,isupv-api.rusagrotrans.ru,isupv-dev.rusagrotrans.ru}"
    export NO_PROXY="$no_proxy"
}

# Для Geofabrik / внешних загрузок — снять ловушку proxy.
clear_proxy() {
    unset http_proxy https_proxy all_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY
}

extend_no_proxy() {
    local host
    for host in "$@"; do
        [ -n "$host" ] || continue
        case ",$no_proxy," in
            *,"$host",*) ;;
            *)
                no_proxy="${no_proxy},${host}"
                NO_PROXY="$no_proxy"
                export no_proxy NO_PROXY
                ;;
        esac
    done
}

# $1 = dev|prod|staging; $2 = require_mssql (true|false, default false)
load_infisical_secrets() {
    local env_name="${1:?env name}"
    local require_mssql="${2:-false}"
    local token_ref

    export INFISICAL_API_URL="${INFISICAL_API_URL:-http://127.0.0.1:9000}"

    token_ref="$(keyctl search @u user infisical_optim_token 2>/dev/null || true)"
    if [ -z "$token_ref" ]; then
        echo "Ошибка: токен Infisical не найден в User Keyring (@u). Запустите auth-infisical.sh." >&2
        exit 1
    fi

    export INFISICAL_TOKEN
    INFISICAL_TOKEN="$(keyctl pipe "$token_ref")"
    export INFISICAL_TELEMETRY_OFF=true
    export INFISICAL_CHECK_UPDATE=false
    export INFISICAL_DISABLE_UPDATE_CHECK=true

    echo "--- Загрузка секретов Infisical (env=$env_name) ---"
    local start_time end_time
    start_time=$(date +%s)
    eval "$(
        infisical secrets \
            --env "$env_name" \
            --path / \
            --recursive \
            --projectId "$STATIONS_PROJECT_ID" \
            --output dotenv \
        | sed "s/=\(.*\)/='\1'/;s/^/export /"
    )"
    unset INFISICAL_TOKEN
    end_time=$(date +%s)
    echo "Секреты загружены за $((end_time - start_time)) сек."

    if [ "$require_mssql" = "true" ] && [ -z "${MSSQL_SERVER_MSKASUVPL:-}" ]; then
        echo "Ошибка: после Infisical не задан MSSQL_SERVER_MSKASUVPL" >&2
        exit 1
    fi
}

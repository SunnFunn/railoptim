# PYTHONPATH для пакета stations_etl (source, не запускать напрямую).

_STATIONS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

stations_setup_python() {
    export PYTHONPATH="${_STATIONS_DIR}${PYTHONPATH:+:$PYTHONPATH}"
}

run_stations_python() {
    stations_setup_python
    python3 "$@"
}

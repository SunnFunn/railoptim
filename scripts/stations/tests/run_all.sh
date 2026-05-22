#!/bin/bash
# Запуск всех offline-тестов stations ETL.

set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../_python_env.sh
source "$dir/../_python_env.sh"

stations_setup_python
for script in run_parity_tests.py run_nsi_tests.py run_osm_tests.py run_sbin_tests.py run_geo_tests.py; do
    run_stations_python "$dir/$script"
done

#!/bin/bash
# Tier 2: osm.sbin.ru/osm2esr.csv → sbin_esr_index.parquet (интернет, без proxy-trap).
#
#   ./scripts/stations/build-sbin.sh
#   ./scripts/stations/build-sbin.sh --index   # только из cache CSV

set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$dir/_infisical_env.sh"
cd "$(stations_repo_root)"
clear_proxy

run_stations_python "$dir/bin/build_sbin_index.py" "$@"

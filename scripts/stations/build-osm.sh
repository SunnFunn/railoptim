#!/bin/bash
# Geofabrik PBF → osm_esr_index.parquet (без proxy-trap).
#
#   ./scripts/stations/build-osm.sh                    # download + index
#   ./scripts/stations/build-osm.sh --index          # только index из cache
#   ./scripts/stations/build-osm.sh --include-optional

set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$dir/_infisical_env.sh"
cd "$(stations_repo_root)"
clear_proxy

run_stations_python "$dir/bin/build_osm_index.py" "$@"

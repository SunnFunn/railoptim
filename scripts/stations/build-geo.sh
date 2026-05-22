#!/bin/bash
# Join NSI + OSM → stations_geo.sqlite (без сети, без Infisical).
#
#   ./scripts/stations/build-geo.sh [args для bin/build_stations_geo.py…]

set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_python_env.sh
source "$dir/_python_env.sh"
cd "$(cd "$dir/../.." && pwd)"

run_stations_python "$dir/bin/build_stations_geo.py" "$@"

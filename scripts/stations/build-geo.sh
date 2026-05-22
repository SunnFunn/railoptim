#!/bin/bash
# Join NSI + OSM → stations_geo.sqlite (без сети, без Infisical).
#
#   ./scripts/stations/build-geo.sh [args для build_stations_geo.py…]

set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$(cd "$dir/../.." && pwd)"

python3 "$dir/build_stations_geo.py" "$@"

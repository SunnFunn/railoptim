#!/bin/bash
# Geofabrik: скачать PBF (без proxy-trap, нужен интернет).
#
#   ./scripts/stations/download-pbf.sh
#   ./scripts/stations/download-pbf.sh --include-optional
#   ./scripts/stations/download-pbf.sh --regions russia,belarus --force-download

set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$dir/_infisical_env.sh"
cd "$(stations_repo_root)"
clear_proxy

run_stations_python "$dir/bin/build_osm_index.py" --download "$@"

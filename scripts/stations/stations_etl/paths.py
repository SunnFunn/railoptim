"""Общие пути артефактов ETL (от корня репозитория)."""

from __future__ import annotations

from pathlib import Path

# scripts/stations/stations_etl/paths.py → repo root
REPO_ROOT = Path(__file__).resolve().parents[3]
DATA_STATIONS = REPO_ROOT / "data/stations"

ESR_COUNTRY_PREFIXES = DATA_STATIONS / "esr_country_prefixes.csv"
MANUAL_COORDS_CSV = DATA_STATIONS / "manual_coords.csv"
GEOFABRIK_MANIFEST = DATA_STATIONS / "geofabrik_regions.yaml"
PBF_CACHE_DIR = DATA_STATIONS / "cache/pbf"

NSI_PARQUET = DATA_STATIONS / "stations_nsi_raw.parquet"
NSI_FETCH_REPORT = DATA_STATIONS / "fetch_report.json"

OSM_INDEX_PARQUET = DATA_STATIONS / "osm_esr_index.parquet"
OSM_INDEX_REPORT = DATA_STATIONS / "osm_index_report.json"

SBIN_INDEX_URL = "http://osm.sbin.ru/esr/osm2esr.csv"
SBIN_CACHE_CSV = DATA_STATIONS / "cache/sbin/osm2esr.csv"
SBIN_INDEX_PARQUET = DATA_STATIONS / "sbin_esr_index.parquet"
SBIN_INDEX_REPORT = DATA_STATIONS / "sbin_index_report.json"

GEO_SQLITE = DATA_STATIONS / "stations_geo.sqlite"
GEO_BUILD_REPORT = DATA_STATIONS / "build_report.json"
GEO_UNMATCHED_CSV = DATA_STATIONS / "unmatched_esr6.csv"
GEO_CROSS_BORDER_CSV = DATA_STATIONS / "cross_border_esr6_conflicts.csv"

#!/usr/bin/env python3
"""Unit-тест join NSI + OSM → SQLite (без прод-данных)."""

from __future__ import annotations

import sqlite3
import sys
import tempfile
from pathlib import Path

from country import EsrCountryIndex
from fetch_nsi_from_mssql import _load_csv_rows
from geo_join import join_nsi_osm, write_sqlite
from nsi_process import process_nsi_rows

ROOT = Path(__file__).resolve().parents[2]
SAMPLE = Path(__file__).resolve().parent / "test_nsi_sample.csv"
PREFIXES = ROOT / "data/stations/esr_country_prefixes.csv"


def main() -> int:
    rows = _load_csv_rows(SAMPLE)
    index = EsrCountryIndex.load(PREFIXES)
    nsi_records, _ = process_nsi_rows(rows, index, source="csv:test")
    nsi_rows = [r.as_dict() for r in nsi_records]

    osm_rows = [
        {
            "esr6": "194013",
            "lat": 55.7558,
            "lon": 37.6173,
            "source": "osm_pbf",
            "match_method": "ref",
            "tag_name": "ref",
            "osm_id": 100,
            "name_osm": "Moscow",
            "pbf_region": "russia",
            "region_group": "ru",
            "confidence": 1.0,
        },
        {
            "esr6": "160001",
            "lat": 52.0976,
            "lon": 23.7341,
            "source": "osm_pbf",
            "match_method": "ref",
            "tag_name": "ref",
            "osm_id": 200,
            "name_osm": "Brest",
            "pbf_region": "belarus",
            "region_group": "cis",
            "confidence": 1.0,
        },
        {
            "esr6": "210001",
            "lat": 56.9496,
            "lon": 24.1052,
            "source": "osm_pbf",
            "match_method": "uic_ref",
            "tag_name": "uic_ref",
            "osm_id": 300,
            "name_osm": "Riga",
            "pbf_region": "latvia",
            "region_group": "baltic",
            "confidence": 1.0,
        },
    ]

    join = join_nsi_osm(nsi_rows, osm_rows, built_at="2026-01-01T00:00:00+00:00")
    assert len(join.rows) == 3
    assert len(join.unmatched) == 3  # 6 nsi - 3 matched
    by_esr = {r.esr6: r for r in join.rows}
    assert by_esr["194013"].name == "Москва-Пассажирская-Казанская (полное имя)"
    assert by_esr["194013"].region_group == "ru"
    assert "063000" in {u["esr6"] for u in join.unmatched}

    with tempfile.TemporaryDirectory() as td:
        db = Path(td) / "geo.sqlite"
        write_sqlite(join.rows, db)
        conn = sqlite3.connect(db)
        try:
            cnt = conn.execute("SELECT COUNT(*) FROM stations_geo").fetchone()[0]
            assert cnt == 3
            row = conn.execute(
                "SELECT name, lat, lon FROM stations_geo WHERE esr6='160001'"
            ).fetchone()
            assert row[0] == "Брест-Центральный"
        finally:
            conn.close()

    print("geo join OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

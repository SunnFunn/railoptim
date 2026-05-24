#!/usr/bin/env python3
"""Unit-тест join NSI + OSM → SQLite (без прод-данных)."""

from __future__ import annotations

import sqlite3
import sys
import tempfile
from pathlib import Path

from stations_etl.country import EsrCountryIndex
from stations_etl.geo.join import join_nsi_osm, write_sqlite
from stations_etl.nsi.parquet_io import load_csv_rows
from stations_etl.nsi.process import process_nsi_rows
from stations_etl.paths import ESR_COUNTRY_PREFIXES

SAMPLE = Path(__file__).resolve().parent / "fixtures" / "test_nsi_sample.csv"


def main() -> int:
    rows = load_csv_rows(SAMPLE)
    index = EsrCountryIndex.load(ESR_COUNTRY_PREFIXES)
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
    assert len(join.unmatched) == 3
    by_esr = {r.esr6: r for r in join.rows}
    assert by_esr["194013"].name == "Москва-Пассажирская-Казанская (полное имя)"
    assert by_esr["194013"].region_group == "ru"
    assert by_esr["194013"].source == "osm_pbf"
    assert "063000" in {u["esr6"] for u in join.unmatched}

    sbin_rows = [
        {
            "esr6": "063000",
            "lat": 55.1,
            "lon": 37.2,
            "source": "osm_sbin",
            "match_method": "osm2esr_csv",
            "osm_id": 999001,
            "name_osm": "Тест-SBIN",
            "confidence": 0.95,
        }
    ]
    join2 = join_nsi_osm(
        nsi_rows, osm_rows, sbin_rows, built_at="2026-01-01T00:00:00+00:00"
    )
    assert len(join2.rows) == 4
    assert len(join2.unmatched) == 2
    sbin_match = next(r for r in join2.rows if r.esr6 == "063000")
    assert sbin_match.source == "osm_sbin"
    assert sbin_match.name == "Пенза III"
    # Tier1 wins over Tier2 for same esr6
    assert by_esr["194013"].source == "osm_pbf"
    assert next(r for r in join2.rows if r.esr6 == "194013").source == "osm_pbf"

    manual_rows = {
        "570001": {
            "esr6": "570001",
            "lat": 40.4093,
            "lon": 49.8671,
            "source": "manual",
            "match_method": "manual_csv",
            "osm_id": None,
            "name_osm": "Баку ручная",
            "confidence": 1.0,
        }
    }
    join3 = join_nsi_osm(
        nsi_rows, osm_rows, sbin_rows, manual_rows, built_at="2026-01-01T00:00:00+00:00"
    )
    assert len(join3.rows) == 5
    assert len(join3.unmatched) == 1
    manual_match = next(r for r in join3.rows if r.esr6 == "570001")
    assert manual_match.source == "manual"
    assert manual_match.lat == 40.4093
    assert "570001" not in {u["esr6"] for u in join3.unmatched}

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

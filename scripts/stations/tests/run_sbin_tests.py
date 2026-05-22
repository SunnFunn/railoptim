#!/usr/bin/env python3
"""Unit-тесты парсинга osm.sbin.ru osm2esr.csv."""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

from stations_etl.osm.sbin import build_sbin_index_rows, merge_sbin_candidates, parse_osm2esr_csv

FIXTURE = Path(__file__).resolve().parent / "fixtures" / "test_sbin_sample.csv"


def test_parse_and_merge() -> None:
    text = FIXTURE.read_text(encoding="utf-8")
    candidates = parse_osm2esr_csv(text)
    assert len(candidates) == 3

    merged = merge_sbin_candidates(candidates)
    assert len(merged) == 2
    # PBF-like priority: station beats halt for duplicate 194013
    assert merged["194013"].railway == "station"
    assert merged["194013"].osm_id == 999003
    assert merged["063000"].lat == 55.1


def test_build_parquet_roundtrip() -> None:
    with tempfile.TemporaryDirectory() as td:
        csv_path = Path(td) / "osm2esr.csv"
        csv_path.write_text(FIXTURE.read_text(encoding="utf-8"), encoding="utf-8")
        rows, report = build_sbin_index_rows(csv_path, download=False)
        assert report["sbin_unique_esr6"] == 2
        assert rows[0]["source"] == "osm_sbin"
        assert rows[0]["match_method"] == "osm2esr_csv"


def main() -> int:
    test_parse_and_merge()
    test_build_parquet_roundtrip()
    print("sbin extract OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

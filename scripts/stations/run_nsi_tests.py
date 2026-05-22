#!/usr/bin/env python3
"""Тест process_nsi_rows / fetch без MSSQL (test_nsi_sample.csv)."""

from __future__ import annotations

import csv
import json
import sys
import tempfile
from pathlib import Path

from country import EsrCountryIndex
from fetch_nsi_from_mssql import _load_csv_rows, _write_parquet
from nsi_process import process_nsi_rows

ROOT = Path(__file__).resolve().parents[2]
SAMPLE = Path(__file__).resolve().parent / "test_nsi_sample.csv"
PREFIXES = ROOT / "data/stations/esr_country_prefixes.csv"


def main() -> int:
    rows = _load_csv_rows(SAMPLE)
    index = EsrCountryIndex.load(PREFIXES)
    records, report = process_nsi_rows(rows, index, source="csv:test")

    assert report["nsi_total"] == 9
    assert report["nsi_unique_esr6"] == 6
    assert report["nsi_rejected"] == 2
    assert report["nsi_duplicate_esr6_count"] == 1

    by_esr = {r.esr6: r for r in records}
    assert by_esr["194013"].region_group == "ru"
    assert by_esr["160001"].country_hint == "BY"
    assert by_esr["210001"].region_group == "baltic"
    assert by_esr["570001"].region_group == "south_caucasus"
    assert by_esr["063000"].esr6 == "063000"
    assert by_esr["001234"].esr6 == "001234"

    with tempfile.TemporaryDirectory() as td:
        pq = Path(td) / "out.parquet"
        _write_parquet(records, pq)
        try:
            import pyarrow.parquet as pq_mod

            table = pq_mod.read_table(pq)
            assert table.num_rows == 6
            assert "country_hint" in table.column_names

            import subprocess

            r = subprocess.run(
                [sys.executable, str(Path(__file__).resolve().parent / "sample_nsi_parquet.py"),
                 "--input", str(pq), "--n", "6", "--seed", "1", "--check"],
                capture_output=True,
                text=True,
            )
            if r.returncode != 0:
                print(r.stdout, r.stderr, file=sys.stderr)
                raise AssertionError("sample_nsi_parquet --check failed")
        except ImportError:
            print("skip parquet read: pyarrow not installed", file=sys.stderr)

    print("nsi process OK")
    print(json.dumps(report["nsi_by_region_group"], ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())

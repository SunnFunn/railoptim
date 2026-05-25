#!/usr/bin/env python3
"""Тест process_nsi_rows / fetch без MSSQL (test_nsi_sample.csv)."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path

from stations_etl.country import EsrCountryIndex
from stations_etl.nsi.parquet_io import load_csv_rows, write_parquet
from stations_etl.nsi.process import process_nsi_rows
from stations_etl.paths import ESR_COUNTRY_PREFIXES

SAMPLE = Path(__file__).resolve().parent / "fixtures" / "test_nsi_sample.csv"
TOOLS_DIR = Path(__file__).resolve().parents[1] / "tools"


def main() -> int:
    rows = load_csv_rows(SAMPLE)
    index = EsrCountryIndex.load(ESR_COUNTRY_PREFIXES)
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
    assert by_esr["194013"].railway_rw == "МСК"
    assert by_esr["063000"].railway_rw == "КБШ"
    assert by_esr["160001"].railway_rw == "БЕЛ"

    with tempfile.TemporaryDirectory() as td:
        pq = Path(td) / "out.parquet"
        write_parquet(records, pq)
        try:
            import pyarrow.parquet as pq_mod

            table = pq_mod.read_table(pq)
            assert table.num_rows == 6
            assert "country_hint" in table.column_names

            r = subprocess.run(
                [
                    sys.executable,
                    str(TOOLS_DIR / "sample_nsi_parquet.py"),
                    "--input",
                    str(pq),
                    "--n",
                    "6",
                    "--seed",
                    "1",
                    "--check",
                ],
                capture_output=True,
                text=True,
                env={**dict(__import__("os").environ), "PYTHONPATH": str(Path(__file__).resolve().parents[1])},
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

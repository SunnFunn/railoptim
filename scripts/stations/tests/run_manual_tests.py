#!/usr/bin/env python3
"""Unit-тесты Tier0 manual_coords.csv."""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

from stations_etl.geo.manual import load_manual_coord_records, load_manual_coords


def test_load_manual_csv() -> None:
    with tempfile.TemporaryDirectory() as td:
        path = Path(td) / "manual.csv"
        path.write_text(
            "# comment\n"
            "esr6,lat,lon,note\n"
            "520101,44.723,37.769,Новороссийск\n"
            "63000,55.0,37.0,без ведущих нулей\n",
            encoding="utf-8",
        )
        rows = load_manual_coords(path)
        assert len(rows) == 2
        assert "520101" in rows
        assert rows["063000"]["lat"] == 55.0
        recs = load_manual_coord_records(path)
        assert recs[0].esr6 == "063000"


def main() -> int:
    test_load_manual_csv()
    print("manual coords OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

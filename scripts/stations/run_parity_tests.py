#!/usr/bin/env python3
"""Проверка паритета normalize/checksum/country с test_normalize_parity.json."""

from __future__ import annotations

import json
import sys
from pathlib import Path

from country import EsrCountryIndex
from normalize import normalize_esr6, validate_esr6_checksum

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = Path(__file__).resolve().parent / "test_normalize_parity.json"


def main() -> int:
    data = json.loads(FIXTURE.read_text(encoding="utf-8"))
    errors = 0

    for row in data["normalize"]:
        inp, want = row[0], row[1]
        got = normalize_esr6(inp)
        if got != want:
            print(f"normalize FAIL: {inp!r} -> {got!r}, want {want!r}")
            errors += 1

    for code in data["checksum_valid"]:
        if not validate_esr6_checksum(code):
            print(f"checksum FAIL valid: {code}")
            errors += 1

    for code in data["checksum_invalid"]:
        if validate_esr6_checksum(code):
            print(f"checksum FAIL invalid: {code}")
            errors += 1

    idx = EsrCountryIndex.load(ROOT / "data/stations/esr_country_prefixes.csv")
    assert idx.classify("160001") is not None
    assert idx.classify("160001").country_hint == "BY"
    assert idx.classify("194013").region_group == "ru"
    assert idx.classify("210001").region_group == "baltic"

    if errors:
        print(f"{errors} error(s)")
        return 1
    print("parity OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""
Выгрузка NSI.Station → data/stations/stations_nsi_raw.parquet + fetch_report.json.

MSSQL (как dislocations.py):
  MSSQL_SERVER / MSSQL_HOST / MSSQL_SERVER_MSKASUVPL
  MSSQL_USER / DOMAIN_USER
  MSSQL_PASSWORD / PASSWORD
  MSSQL_DATABASE / MSSQL_DB_ASUVP
  MSSQL_DOMAIN (опционально, префикс логина)

Запрос: SELECT Code6, Name FROM NSI.Station (NOLOCK);

Для теста без БД: --input-csv path/to/sample.csv (колонки Code6,Name).
"""

from __future__ import annotations

import argparse
import csv
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_OUT = ROOT / "data/stations/stations_nsi_raw.parquet"
DEFAULT_REPORT = ROOT / "data/stations/fetch_report.json"
DEFAULT_PREFIXES = ROOT / "data/stations/esr_country_prefixes.csv"


def _load_csv_rows(path: Path) -> list[tuple[str, str]]:
    rows: list[tuple[str, str]] = []
    with path.open(encoding="utf-8-sig", newline="") as f:
        reader = csv.DictReader(f)
        if not reader.fieldnames:
            raise SystemExit(f"пустой CSV: {path}")
        fields = {h.strip().lower(): h for h in reader.fieldnames}
        code_key = fields.get("code6")
        name_key = fields.get("name")
        if not code_key or not name_key:
            raise SystemExit(f"CSV {path}: нужны колонки Code6 и Name")
        for row in reader:
            rows.append((row.get(code_key, ""), row.get(name_key, "")))
    return rows


def _write_parquet(records: list, path: Path) -> None:
    try:
        import pyarrow as pa
        import pyarrow.parquet as pq
    except ImportError as e:
        raise SystemExit(
            "fetch_nsi: установите pyarrow (scripts/stations/requirements-stations.txt)"
        ) from e

    path.parent.mkdir(parents=True, exist_ok=True)
    table = pa.Table.from_pylist([r.as_dict() for r in records])
    pq.write_table(table, path, compression="zstd")


def main() -> int:
    parser = argparse.ArgumentParser(description="NSI.Station → parquet")
    parser.add_argument("--output", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--report", type=Path, default=DEFAULT_REPORT)
    parser.add_argument("--prefixes", type=Path, default=DEFAULT_PREFIXES)
    parser.add_argument(
        "--input-csv",
        type=Path,
        help="вместо MSSQL — чтение CSV (Code6,Name) для теста",
    )
    args = parser.parse_args()

    from country import EsrCountryIndex
    from mssql import fetch_nsi_station_rows
    from nsi_process import process_nsi_rows

    if args.input_csv:
        rows = _load_csv_rows(args.input_csv)
        source = f"csv:{args.input_csv.name}"
    else:
        rows = fetch_nsi_station_rows()
        source = "mssql"

    index = EsrCountryIndex.load(args.prefixes)
    records, report = process_nsi_rows(rows, index, source=source)
    report["output_parquet"] = str(args.output)
    report["report_path"] = str(args.report)

    _write_parquet(records, args.output)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    print(
        f"fetch_nsi: {report['nsi_total']} rows → {report['nsi_unique_esr6']} unique esr6 "
        f"({report['nsi_rejected']} rejected, {report['nsi_duplicate_esr6_count']} duplicate esr6)",
        file=sys.stderr,
    )
    for rg, cnt in report["nsi_by_region_group"].items():
        print(f"  {rg}: {cnt}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    sys.exit(main())

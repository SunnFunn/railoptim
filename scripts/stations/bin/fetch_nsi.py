#!/usr/bin/env python3
"""
Выгрузка NSI.Station → data/stations/stations_nsi_raw.parquet + fetch_report.json.

MSSQL (те же секреты Infisical, что dislocations.py / wash.py):
  MSSQL_SERVER_MSKASUVPL, DOMAIN_USER, PASSWORD, MSSQL_DB_ASUVP, MSSQL_DOMAIN

Загрузка env: ./scripts/stations/run.sh prod fetch-nsi

Для теста без БД: --input-csv path/to/sample.csv (колонки Code6, Name; опционально ShortName).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from stations_etl.country import EsrCountryIndex
from stations_etl.nsi.mssql import fetch_nsi_station_rows
from stations_etl.nsi.parquet_io import load_csv_rows, write_parquet
from stations_etl.nsi.process import process_nsi_rows
from stations_etl.paths import ESR_COUNTRY_PREFIXES, NSI_FETCH_REPORT, NSI_PARQUET


def main() -> int:
    parser = argparse.ArgumentParser(description="NSI.Station → parquet")
    parser.add_argument("--output", type=Path, default=NSI_PARQUET)
    parser.add_argument("--report", type=Path, default=NSI_FETCH_REPORT)
    parser.add_argument("--prefixes", type=Path, default=ESR_COUNTRY_PREFIXES)
    parser.add_argument(
        "--input-csv",
        type=Path,
        help="вместо MSSQL — чтение CSV (Code6,Name) для теста",
    )
    args = parser.parse_args()

    if args.input_csv:
        rows = load_csv_rows(args.input_csv)
        source = f"csv:{args.input_csv.name}"
    else:
        rows = fetch_nsi_station_rows()
        source = "mssql"

    index = EsrCountryIndex.load(args.prefixes)
    records, report = process_nsi_rows(rows, index, source=source)
    report["output_parquet"] = str(args.output)
    report["report_path"] = str(args.report)

    write_parquet(records, args.output)
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

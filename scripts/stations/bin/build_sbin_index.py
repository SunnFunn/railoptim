#!/usr/bin/env python3
"""
Tier 2: osm.sbin.ru/osm2esr.csv → sbin_esr_index.parquet.

  python3 bin/build_sbin_index.py              # download + index
  python3 bin/build_sbin_index.py --index    # только из cache CSV
  python3 bin/build_sbin_index.py --force-download
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from stations_etl.osm.sbin import build_sbin_index_rows
from stations_etl.paths import SBIN_CACHE_CSV, SBIN_INDEX_PARQUET, SBIN_INDEX_REPORT


def _write_parquet(rows: list[dict], path: Path) -> None:
    try:
        import pyarrow as pa
        import pyarrow.parquet as pq
    except ImportError as e:
        raise SystemExit(
            "build_sbin: нужен pyarrow (scripts/stations/requirements-stations.txt)"
        ) from e
    path.parent.mkdir(parents=True, exist_ok=True)
    table = pa.Table.from_pylist(rows)
    pq.write_table(table, path, compression="zstd")


def main() -> int:
    parser = argparse.ArgumentParser(description="osm.sbin.ru osm2esr.csv → parquet")
    parser.add_argument("--csv", type=Path, default=SBIN_CACHE_CSV, help="локальный CSV")
    parser.add_argument("--output", type=Path, default=SBIN_INDEX_PARQUET)
    parser.add_argument("--report", type=Path, default=SBIN_INDEX_REPORT)
    parser.add_argument("--download", action="store_true", help="скачать CSV")
    parser.add_argument("--index", action="store_true", help="построить индекс из cache")
    parser.add_argument("--force-download", action="store_true")
    args = parser.parse_args()

    do_download = args.download or not args.index
    do_index = args.index or not args.download
    if args.download and args.index:
        do_download = do_index = True

    if not do_index:
        if do_download:
            from stations_etl.osm.sbin import download_osm2esr_csv

            download_osm2esr_csv(args.csv, force=args.force_download)
        return 0

    rows, report = build_sbin_index_rows(
        args.csv,
        download=do_download,
        force_download=args.force_download,
    )
    _write_parquet(rows, args.output)
    report["output_parquet"] = str(args.output)
    report["report_path"] = str(args.report)

    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    print(
        f"build_sbin: {report['candidates_total']} candidates → "
        f"{report['sbin_unique_esr6']} unique esr6",
        file=sys.stderr,
    )
    print(f"  parquet: {args.output}", file=sys.stderr)
    print(f"  report:  {args.report}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

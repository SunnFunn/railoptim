#!/usr/bin/env python3
"""
Join stations_nsi_raw.parquet + osm_esr_index.parquet → stations_geo.sqlite.

  python3 build_stations_geo.py
  python3 build_stations_geo.py --nsi /path/nsi.parquet --osm /path/osm.parquet
"""

from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_NSI = ROOT / "data/stations/stations_nsi_raw.parquet"
DEFAULT_OSM = ROOT / "data/stations/osm_esr_index.parquet"
DEFAULT_SQLITE = ROOT / "data/stations/stations_geo.sqlite"
DEFAULT_REPORT = ROOT / "data/stations/build_report.json"
DEFAULT_UNMATCHED = ROOT / "data/stations/unmatched_esr6.csv"
DEFAULT_CROSS_BORDER = ROOT / "data/stations/cross_border_esr6_conflicts.csv"


def main() -> int:
    parser = argparse.ArgumentParser(description="NSI + OSM → stations_geo.sqlite")
    parser.add_argument("--nsi", type=Path, default=DEFAULT_NSI)
    parser.add_argument("--osm", type=Path, default=DEFAULT_OSM)
    parser.add_argument("--output", type=Path, default=DEFAULT_SQLITE)
    parser.add_argument("--report", type=Path, default=DEFAULT_REPORT)
    parser.add_argument("--unmatched", type=Path, default=DEFAULT_UNMATCHED)
    parser.add_argument("--cross-border", type=Path, default=DEFAULT_CROSS_BORDER)
    args = parser.parse_args()

    from geo_join import (
        build_report,
        join_nsi_osm,
        load_parquet_rows,
        write_cross_border_csv,
        write_sqlite,
        write_unmatched_csv,
    )

    built_at = datetime.now(timezone.utc).isoformat()
    nsi_rows = load_parquet_rows(args.nsi)
    osm_rows = load_parquet_rows(args.osm)

    join = join_nsi_osm(nsi_rows, osm_rows, built_at=built_at)
    write_sqlite(join.rows, args.output)
    write_unmatched_csv(join.unmatched, args.unmatched)
    if join.cross_border:
        write_cross_border_csv(join.cross_border, args.cross_border)

    report = build_report(
        join,
        nsi_rows,
        osm_rows,
        built_at=built_at,
        paths={
            "nsi_parquet": str(args.nsi),
            "osm_parquet": str(args.osm),
            "output_sqlite": str(args.output),
            "unmatched_csv": str(args.unmatched),
            "cross_border_csv": str(args.cross_border) if join.cross_border else None,
        },
    )
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    print(
        f"build_geo: {report['matched_with_coords']}/{report['nsi_unique_esr6']} "
        f"({report['coverage_pct']}%) → {args.output}",
        file=sys.stderr,
    )
    for rg, stats in report["coverage_by_region_group"].items():
        print(
            f"  {rg}: {stats['matched']}/{stats['total']} ({stats['coverage_pct']}%)",
            file=sys.stderr,
        )
    print(f"  unmatched: {report['unmatched_count']} → {args.unmatched}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

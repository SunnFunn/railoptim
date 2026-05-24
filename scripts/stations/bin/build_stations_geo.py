#!/usr/bin/env python3
"""
Join NSI + OSM (Tier1) + sbin (Tier2 fallback) → stations_geo.sqlite.

  python3 bin/build_stations_geo.py
  python3 bin/build_stations_geo.py --no-sbin   # только OSM PBF
"""

from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

from stations_etl.geo.join import (
    build_report,
    join_nsi_osm,
    load_parquet_rows,
    write_cross_border_csv,
    write_sqlite,
    write_unmatched_csv,
)
from stations_etl.geo.manual import load_manual_coords
from stations_etl.paths import (
    GEO_BUILD_REPORT,
    GEO_CROSS_BORDER_CSV,
    GEO_SQLITE,
    GEO_UNMATCHED_CSV,
    MANUAL_COORDS_CSV,
    NSI_PARQUET,
    OSM_INDEX_PARQUET,
    SBIN_INDEX_PARQUET,
)


def main() -> int:
    parser = argparse.ArgumentParser(description="NSI + OSM + sbin → stations_geo.sqlite")
    parser.add_argument("--nsi", type=Path, default=NSI_PARQUET)
    parser.add_argument("--osm", type=Path, default=OSM_INDEX_PARQUET)
    parser.add_argument("--sbin", type=Path, default=SBIN_INDEX_PARQUET)
    parser.add_argument("--no-sbin", action="store_true", help="не использовать Tier2 sbin")
    parser.add_argument("--manual", type=Path, default=MANUAL_COORDS_CSV, help="Tier0 manual CSV")
    parser.add_argument("--output", type=Path, default=GEO_SQLITE)
    parser.add_argument("--report", type=Path, default=GEO_BUILD_REPORT)
    parser.add_argument("--unmatched", type=Path, default=GEO_UNMATCHED_CSV)
    parser.add_argument("--cross-border", type=Path, default=GEO_CROSS_BORDER_CSV)
    args = parser.parse_args()

    built_at = datetime.now(timezone.utc).isoformat()
    nsi_rows = load_parquet_rows(args.nsi)
    osm_rows = load_parquet_rows(args.osm)

    sbin_rows: list[dict] = []
    sbin_path: Path | None = None
    if not args.no_sbin:
        if args.sbin.is_file():
            sbin_rows = load_parquet_rows(args.sbin)
            sbin_path = args.sbin
        else:
            print(
                f"build_geo: sbin index не найден ({args.sbin}), только Tier1 OSM PBF",
                file=sys.stderr,
            )

    manual_rows = load_manual_coords(args.manual)
    if manual_rows:
        print(f"build_geo: Tier0 manual coords: {len(manual_rows)} из {args.manual}", file=sys.stderr)
    elif args.manual.is_file():
        print(f"build_geo: Tier0 manual coords: 0 (только заголовок) в {args.manual}", file=sys.stderr)

    join = join_nsi_osm(
        nsi_rows,
        osm_rows,
        sbin_rows or None,
        manual_rows,
        built_at=built_at,
    )
    write_sqlite(join.rows, args.output)
    write_unmatched_csv(join.unmatched, args.unmatched)
    if join.cross_border:
        write_cross_border_csv(join.cross_border, args.cross_border)

    report = build_report(
        join,
        nsi_rows,
        osm_rows,
        sbin_rows or None,
        manual_rows,
        built_at=built_at,
        paths={
            "nsi_parquet": str(args.nsi),
            "osm_parquet": str(args.osm),
            "sbin_parquet": str(sbin_path) if sbin_path else None,
            "manual_coords_csv": str(args.manual) if manual_rows or args.manual.is_file() else None,
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
    print(
        f"  tier1 osm_pbf: {report['matched_via_osm_pbf']}, "
        f"tier2 osm_sbin: {report['matched_via_osm_sbin']}, "
        f"tier0 manual: {report['matched_via_manual']}",
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

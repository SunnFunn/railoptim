#!/usr/bin/env python3
"""
Geofabrik PBF → osm_esr_index.parquet (esr6 → lat/lon из OSM).

  python3 build_osm_esr_index.py --download              # только скачать PBF
  python3 build_osm_esr_index.py --index                 # индекс из cache
  python3 build_osm_esr_index.py                         # download + index
  python3 build_osm_esr_index.py --regions russia,belarus
  python3 build_osm_esr_index.py --download --include-optional   # china-latest
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MANIFEST = ROOT / "data/stations/geofabrik_regions.yaml"
DEFAULT_OUTPUT = ROOT / "data/stations/osm_esr_index.parquet"
DEFAULT_REPORT = ROOT / "data/stations/osm_index_report.json"


def _write_parquet(rows: list[dict], path: Path) -> None:
    try:
        import pyarrow as pa
        import pyarrow.parquet as pq
    except ImportError as e:
        raise SystemExit(
            "build_osm: нужен pyarrow (scripts/stations/requirements-stations.txt)"
        ) from e
    path.parent.mkdir(parents=True, exist_ok=True)
    table = pa.Table.from_pylist(rows)
    pq.write_table(table, path, compression="zstd")


def main() -> int:
    parser = argparse.ArgumentParser(description="OSM PBF → esr6 index")
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--report", type=Path, default=DEFAULT_REPORT)
    parser.add_argument("--download", action="store_true", help="скачать PBF по манифесту")
    parser.add_argument("--index", action="store_true", help="построить индекс из cache")
    parser.add_argument("--force-download", action="store_true")
    parser.add_argument(
        "--include-optional",
        action="store_true",
        help="скачивать optional регионы (china-latest ~1.3 GB)",
    )
    parser.add_argument(
        "--regions",
        type=str,
        default="",
        help="через запятую id из geofabrik_regions.yaml (по умолчанию все)",
    )
    args = parser.parse_args()

    do_download = args.download or not args.index
    do_index = args.index or not args.download
    if args.download and args.index:
        do_download = do_index = True

    from geofabrik import download_regions, load_manifest, pbf_path
    from osm_esr_extract import extract_from_pbf, merge_candidates

    manifest = load_manifest(args.manifest)
    region_ids: set[str] | None = None
    if args.regions.strip():
        region_ids = {x.strip() for x in args.regions.split(",") if x.strip()}

    regions = [
        r
        for r in manifest.regions
        if region_ids is None or r.id in region_ids
    ]
    if not regions:
        raise SystemExit("нет регионов для обработки")

    missing_required: list[str] = []
    for r in regions:
        if r.required and not r.optional and not pbf_path(manifest, r).is_file():
            missing_required.append(r.id)
    if missing_required and do_index and not do_download:
        raise SystemExit(
            f"PBF не найден для required: {', '.join(missing_required)}; запустите с --download"
        )

    if do_download:
        print("=== download PBF ===", flush=True)
        download_regions(
            manifest,
            region_ids={r.id for r in regions},
            include_optional=args.include_optional,
            force=args.force_download,
        )

    if not do_index:
        return 0

    print("=== extract OSM → esr6 ===", flush=True)
    all_candidates = []
    processed: list[str] = []
    skipped: list[str] = []

    for region in sorted(regions, key=lambda r: r.priority):
        if region.optional and not args.include_optional:
            skipped.append(region.id)
            continue
        path = pbf_path(manifest, region)
        if not path.is_file():
            if region.required:
                raise SystemExit(f"required PBF отсутствует: {path}")
            print(f"  skip {region.id}: нет файла {path.name}", flush=True)
            skipped.append(region.id)
            continue
        print(f"  extract {region.id} ← {path.name}", flush=True)
        batch = extract_from_pbf(path, region)
        print(f"    candidates: {len(batch)}", flush=True)
        all_candidates.extend(batch)
        processed.append(region.id)

    merged = merge_candidates(all_candidates)
    rows = [c.as_dict() for c in sorted(merged.index.values(), key=lambda x: x.esr6)]

    _write_parquet(rows, args.output)

    by_region = Counter(c.pbf_region for c in merged.index.values())
    by_tag = Counter(c.tag_name for c in merged.index.values())
    by_group = Counter(c.region_group for c in merged.index.values())

    report = {
        "built_at": datetime.now(timezone.utc).isoformat(),
        "manifest": str(args.manifest),
        "output_parquet": str(args.output),
        "regions_processed": processed,
        "regions_skipped": skipped,
        "candidates_total": merged.candidates_total,
        "osm_unique_esr6": len(rows),
        "ambiguous_count": len(merged.ambiguous),
        "cross_border_count": len(merged.cross_border),
        "by_pbf_region": dict(sorted(by_region.items())),
        "by_match_method": dict(sorted(by_tag.items())),
        "by_region_group": dict(sorted(by_group.items())),
        "ambiguous_esr6": merged.ambiguous[:500],
        "cross_border_esr6": merged.cross_border[:500],
    }
    if len(merged.ambiguous) > 500:
        report["ambiguous_truncated"] = len(merged.ambiguous) - 500
    if len(merged.cross_border) > 500:
        report["cross_border_truncated"] = len(merged.cross_border) - 500

    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    print(
        f"build_osm: {merged.candidates_total} candidates → {len(rows)} unique esr6",
        file=sys.stderr,
    )
    print(f"  parquet: {args.output}", file=sys.stderr)
    print(f"  report:  {args.report}", file=sys.stderr)
    for rg, cnt in sorted(by_group.items()):
        print(f"  {rg}: {cnt}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    sys.exit(main())

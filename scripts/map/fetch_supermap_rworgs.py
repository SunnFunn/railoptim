#!/usr/bin/env python3
"""
Скачать полигоны ж/д дорог с Supermap (GeoServer WFS) → data/map/railways_zones.geojson.

Источник: https://supermap.zatramvaj.su/  слой Supermap_GeoServer:rworgs
Маппинг имён: data/map/supermap_rw_name_to_rw.csv
Фильтр кодов: data/map/railway_rw_allowlist.txt

Пример:
  ./scripts/map/run.sh fetch-zones
  uv run python fetch_supermap_rworgs.py --dry-run
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_WFS = "https://wms.zatramvaj.su/geoserver/ows"
DEFAULT_RAW = ROOT / "data/map/supermap_rworgs_raw.geojson"
DEFAULT_OUT = ROOT / "data/map/railways_zones.geojson"
DEFAULT_REPORT = ROOT / "data/map/railways_zones_report.json"
DEFAULT_ALLOWLIST = ROOT / "data/map/railway_rw_allowlist.txt"
DEFAULT_NAME_MAP = ROOT / "data/map/supermap_rw_name_to_rw.csv"


def load_allowlist(path: Path) -> set[str]:
    codes: set[str] = set()
    with path.open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            codes.add(line.upper())
    return codes


def load_name_map(path: Path) -> dict[str, str]:
    mapping: dict[str, str] = {}
    with path.open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#") or line.startswith("supermap_name,"):
                continue
            parts = [p.strip() for p in line.split(",")]
            if len(parts) < 2:
                continue
            mapping[parts[0]] = parts[1].upper()
    return mapping


def fetch_wfs_geojson(url: str, *, timeout: float = 120.0) -> dict:
    params = {
        "service": "WFS",
        "version": "2.0.0",
        "request": "GetFeature",
        "typeName": "Supermap_GeoServer:rworgs",
        "outputFormat": "application/json",
        "srsName": "EPSG:4326",
        "count": "200",
    }
    full = f"{url}?{urllib.parse.urlencode(params)}"
    req = urllib.request.Request(full, headers={"User-Agent": "railoptim/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def _bounds_from_geometry(geom: dict) -> tuple[float, float, float, float]:
    """min_lon, min_lat, max_lon, max_lat."""

    def walk(coords, depth: int) -> None:
        if depth == 0:
            lon, lat = coords[0], coords[1]
            nonlocal min_lon, min_lat, max_lon, max_lat
            min_lon = min(min_lon, lon)
            max_lon = max(max_lon, lon)
            min_lat = min(min_lat, lat)
            max_lat = max(max_lat, lat)
            return
        for part in coords:
            walk(part, depth - 1)

    min_lon, min_lat, max_lon, max_lat = 180.0, 90.0, -180.0, -90.0
    gtype = geom.get("type")
    coords = geom.get("coordinates")
    if not coords:
        return min_lon, min_lat, max_lon, max_lat
    if gtype == "Polygon":
        walk(coords, 2)
    elif gtype == "MultiPolygon":
        walk(coords, 3)
    return min_lon, min_lat, max_lon, max_lat


def transform_collection(
    raw: dict,
    *,
    allowlist: set[str],
    name_map: dict[str, str],
) -> tuple[dict, dict]:
    kept: list[dict] = []
    skipped_no_map: list[str] = []
    skipped_not_allowed: list[dict] = []

    for feat in raw.get("features", []):
        props = feat.get("properties") or {}
        sname = (props.get("name") or "").strip()
        if not sname:
            skipped_no_map.append("(empty name)")
            continue
        rw = name_map.get(sname)
        if not rw:
            skipped_no_map.append(sname)
            continue
        if rw not in allowlist:
            skipped_not_allowed.append({"name": sname, "rw": rw})
            continue
        geom = feat.get("geometry")
        if not geom:
            continue
        min_lon, min_lat, max_lon, max_lat = _bounds_from_geometry(geom)
        kept.append(
            {
                "type": "Feature",
                "properties": {
                    "rw": rw,
                    "name_supermap": sname,
                    "name_eng": props.get("nameENG"),
                    "label_lon": min_lon,
                    "label_lat": max_lat,
                },
                "geometry": geom,
            }
        )

    collection = {"type": "FeatureCollection", "features": kept}
    report = {
        "raw_features": len(raw.get("features", [])),
        "kept": len(kept),
        "skipped_no_mapping": skipped_no_map,
        "skipped_not_in_allowlist": skipped_not_allowed,
        "railways": sorted({f["properties"]["rw"] for f in kept}),
    }
    return collection, report


def main() -> int:
    parser = argparse.ArgumentParser(description="Supermap rworgs → railways_zones.geojson")
    parser.add_argument("--wfs-url", default=DEFAULT_WFS)
    parser.add_argument("--raw-out", type=Path, default=DEFAULT_RAW)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--report", type=Path, default=DEFAULT_REPORT)
    parser.add_argument("--allowlist", type=Path, default=DEFAULT_ALLOWLIST)
    parser.add_argument("--name-map", type=Path, default=DEFAULT_NAME_MAP)
    parser.add_argument("--dry-run", action="store_true", help="только скачать и отчёт, не писать out")
    parser.add_argument("--skip-download", action="store_true", help="читать --raw-out с диска")
    args = parser.parse_args()

    allowlist = load_allowlist(args.allowlist)
    name_map = load_name_map(args.name_map)
    if not allowlist:
        print(f"Пустой allowlist: {args.allowlist}", file=sys.stderr)
        return 1
    if not name_map:
        print(f"Пустой name map: {args.name_map}", file=sys.stderr)
        return 1

    if args.skip_download:
        if not args.raw_out.is_file():
            print(f"Нет файла: {args.raw_out}", file=sys.stderr)
            return 1
        raw = json.loads(args.raw_out.read_text(encoding="utf-8"))
    else:
        print(f"WFS: {args.wfs_url} …", file=sys.stderr)
        raw = fetch_wfs_geojson(args.wfs_url)
        args.raw_out.parent.mkdir(parents=True, exist_ok=True)
        with args.raw_out.open("w", encoding="utf-8") as f:
            json.dump(raw, f, ensure_ascii=False, separators=(",", ":"))
        print(f"raw: {args.raw_out} ({len(raw.get('features', []))} features)", file=sys.stderr)

    collection, stats = transform_collection(raw, allowlist=allowlist, name_map=name_map)

    report = {
        "built_at": datetime.now(timezone.utc).isoformat(),
        "source": {
            "site": "https://supermap.zatramvaj.su/",
            "wfs": args.wfs_url,
            "layer": "Supermap_GeoServer:rworgs",
            "raw_file": str(args.raw_out),
        },
        "output": str(args.out),
        "allowlist": str(args.allowlist),
        "name_map": str(args.name_map),
        "stats": stats,
    }

    if not args.dry_run:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        with args.out.open("w", encoding="utf-8") as f:
            json.dump(collection, f, ensure_ascii=False, separators=(",", ":"))
        with args.report.open("w", encoding="utf-8") as f:
            json.dump(report, f, ensure_ascii=False, indent=2)

    print(
        f"OK: {len(collection['features'])} zones → {args.out} "
        f"(raw {stats['raw_features']}, skipped unmapped {len(stats['skipped_no_mapping'])})"
    )
    if stats["skipped_no_mapping"]:
        print("  без маппинга:", ", ".join(stats["skipped_no_mapping"][:15]), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

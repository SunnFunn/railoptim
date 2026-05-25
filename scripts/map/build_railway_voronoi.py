#!/usr/bin/env python3
"""
Пилот: Voronoi-зоны ж/д дорог (3-буквенные коды) → data/map/railways_voronoi.geojson.

Вход:
  - data/stations/stations_geo.sqlite
  - data/stations/stations_nsi_raw.parquet (railway_rw из NSI.RailWay.ShortName)
  - data/stations/esr_district_to_rw.csv (fallback по esr6[:2])

Пример:
  python3 scripts/map/build_railway_voronoi.py
  python3 scripts/map/build_railway_voronoi.py --region ru --bbox 19,35,180,82
"""

from __future__ import annotations

import argparse
import csv
import json
import sqlite3
import sys
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_GEO = ROOT / "data/stations/stations_geo.sqlite"
DEFAULT_NSI = ROOT / "data/stations/stations_nsi_raw.parquet"
DEFAULT_FALLBACK = ROOT / "data/stations/esr_district_to_rw.csv"
DEFAULT_OUT = ROOT / "data/map/railways_voronoi.geojson"
DEFAULT_REPORT = ROOT / "data/map/railways_voronoi_report.json"
DEFAULT_BBOX = (19.0, 35.0, 180.0, 82.0)


def load_esr_fallback(path: Path) -> dict[str, str]:
    mapping: dict[str, str] = {}
    with path.open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if line.startswith("prefix2,"):
                continue
            parts = [p.strip() for p in line.split(",")]
            if len(parts) < 2:
                continue
            mapping[parts[0].zfill(2)[-2:]] = parts[1].upper()
    return mapping


def load_nsi_rw(path: Path) -> dict[str, str | None]:
    if not path.is_file():
        return {}
    try:
        import pyarrow.parquet as pq
    except ImportError:
        print(
            "WARN: pyarrow не установлен — railway_rw только из esr_district_to_rw.csv",
            file=sys.stderr,
        )
        return {}
    table = pq.read_table(path, columns=["esr6", "railway_rw"])
    out: dict[str, str | None] = {}
    for row in table.to_pylist():
        esr6 = str(row.get("esr6") or "").strip()
        rw = row.get("railway_rw")
        if not esr6:
            continue
        rw_s = str(rw).strip().upper() if rw else None
        out[esr6] = rw_s or None
    return out


def load_geo_points(
    geo_path: Path,
    *,
    region: str | None,
) -> list[dict]:
    conn = sqlite3.connect(geo_path)
    try:
        cur = conn.execute(
            """
            SELECT esr6, lat, lon, region_group, country_hint
            FROM stations_geo
            WHERE lat IS NOT NULL AND lon IS NOT NULL
            """
        )
        rows = cur.fetchall()
    finally:
        conn.close()

    points: list[dict] = []
    for esr6, lat, lon, region_group, country_hint in rows:
        if lat is None or lon is None:
            continue
        if region and region != "all" and (region_group or "") != region:
            continue
        points.append(
            {
                "esr6": str(esr6).strip(),
                "lat": float(lat),
                "lon": float(lon),
                "region_group": region_group,
                "country_hint": country_hint,
            }
        )
    return points


def assign_railway_rw(
    points: list[dict],
    nsi_rw: dict[str, str | None],
    fallback: dict[str, str],
) -> tuple[list[dict], dict]:
    stats = Counter()
    skipped: list[dict] = []
    assigned: list[dict] = []

    for p in points:
        esr6 = p["esr6"]
        rw = nsi_rw.get(esr6) if nsi_rw else None
        source = "nsi"
        if not rw and len(esr6) >= 2:
            rw = fallback.get(esr6[:2])
            source = "csv_fallback" if rw else "none"
        if not rw:
            stats["no_rw"] += 1
            skipped.append({"esr6": esr6, "reason": "no_railway_rw"})
            continue
        rw = rw.upper()
        stats[source] += 1
        assigned.append({**p, "railway_rw": rw, "rw_source": source})

    return assigned, {"stats": dict(stats), "skipped_sample": skipped[:100], "skipped_count": len(skipped)}


def build_voronoi_geojson(
    points: list[dict],
    bbox: tuple[float, float, float, float],
    *,
    simplify_tol: float = 0.08,
) -> tuple[dict, dict]:
    from shapely.geometry import MultiPoint, box, mapping
    from shapely.ops import unary_union, voronoi_diagram

    min_lon, min_lat, max_lon, max_lat = bbox
    envelope = box(min_lon, min_lat, max_lon, max_lat)

    coords = [(p["lon"], p["lat"]) for p in points]
    mp = MultiPoint(coords)
    diagram = voronoi_diagram(mp, envelope=envelope)

    if len(diagram.geoms) != len(points):
        raise RuntimeError(
            f"Voronoi: ожидали {len(points)} ячеек, получили {len(diagram.geoms)}"
        )

    by_rw: dict[str, list] = defaultdict(list)
    rw_station_count: Counter[str] = Counter()

    for i, geom in enumerate(diagram.geoms):
        if geom.is_empty:
            continue
        rw = points[i]["railway_rw"]
        clipped = geom.intersection(envelope)
        if clipped.is_empty:
            continue
        if simplify_tol > 0:
            clipped = clipped.simplify(simplify_tol, preserve_topology=True)
        by_rw[rw].append(clipped)
        rw_station_count[rw] += 1

    features = []
    for rw in sorted(by_rw):
        merged = unary_union(by_rw[rw])
        if merged.is_empty:
            continue
        minx, miny, maxx, maxy = merged.bounds
        features.append(
            {
                "type": "Feature",
                "properties": {
                    "rw": rw,
                    "station_count": rw_station_count[rw],
                    "label_lon": minx,
                    "label_lat": maxy,
                },
                "geometry": mapping(merged),
            }
        )

    collection = {"type": "FeatureCollection", "features": features}
    meta = {
        "zone_count": len(features),
        "station_count": len(points),
        "railways": dict(sorted(rw_station_count.items())),
    }
    return collection, meta


def main() -> int:
    parser = argparse.ArgumentParser(description="Voronoi-зоны ж/д дорог → GeoJSON")
    parser.add_argument("--geo", type=Path, default=DEFAULT_GEO)
    parser.add_argument("--nsi", type=Path, default=DEFAULT_NSI)
    parser.add_argument("--fallback", type=Path, default=DEFAULT_FALLBACK)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--report", type=Path, default=DEFAULT_REPORT)
    parser.add_argument(
        "--region",
        default="ru",
        help="фильтр stations_geo.region_group (all — без фильтра)",
    )
    parser.add_argument(
        "--bbox",
        default=",".join(str(x) for x in DEFAULT_BBOX),
        help="min_lon,min_lat,max_lon,max_lat",
    )
    parser.add_argument("--simplify", type=float, default=0.08)
    args = parser.parse_args()

    bbox_parts = [float(x) for x in args.bbox.split(",")]
    if len(bbox_parts) != 4:
        print("bbox: нужно 4 числа", file=sys.stderr)
        return 1
    bbox = tuple(bbox_parts)  # type: ignore[assignment]

    if not args.geo.is_file():
        print(f"Нет файла: {args.geo}", file=sys.stderr)
        return 1

    fallback = load_esr_fallback(args.fallback) if args.fallback.is_file() else {}
    nsi_rw = load_nsi_rw(args.nsi)
    geo_points = load_geo_points(args.geo, region=args.region or None)
    assigned, assign_report = assign_railway_rw(geo_points, nsi_rw, fallback)

    if len(assigned) < 3:
        print(f"Слишком мало станций с railway_rw: {len(assigned)}", file=sys.stderr)
        return 1

    try:
        collection, voronoi_meta = build_voronoi_geojson(
            assigned, bbox, simplify_tol=args.simplify
        )
    except ImportError as e:
        print(
            "Установите зависимости: cd scripts/map && uv sync && ./run.sh build-voronoi",
            file=sys.stderr,
        )
        raise SystemExit(1) from e

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w", encoding="utf-8") as f:
        json.dump(collection, f, ensure_ascii=False, separators=(",", ":"))

    report = {
        "built_at": datetime.now(timezone.utc).isoformat(),
        "region_filter": args.region,
        "bbox": list(bbox),
        "inputs": {
            "geo": str(args.geo),
            "nsi": str(args.nsi) if args.nsi.is_file() else None,
            "fallback": str(args.fallback),
        },
        "output": str(args.out),
        "assignment": assign_report,
        "voronoi": voronoi_meta,
        "disclaimer": "Условные границы (Voronoi по станциям), не официальные границы РЖД",
    }
    with args.report.open("w", encoding="utf-8") as f:
        json.dump(report, f, ensure_ascii=False, indent=2)

    print(f"OK: {args.out} ({len(collection['features'])} zones, {len(assigned)} stations)")
    print(f"report: {args.report}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

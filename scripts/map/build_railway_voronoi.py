#!/usr/bin/env python3
"""
Пилот: Voronoi-зоны ж/д дорог (3-буквенные коды) → data/map/railways_voronoi.geojson.

Вход:
  - data/stations/stations_geo.sqlite
  - data/stations/stations_nsi_raw.parquet (railway_rw из NSI.RailWay.ShortName)
  - data/stations/esr_district_to_rw.csv (fallback только для region_group=ru)
  - data/map/railway_rw_allowlist.txt (какие коды рисовать на карте)

Пример:
  ./scripts/map/run.sh build-voronoi
  ./scripts/map/run.sh build-voronoi --region ru,cis --allowlist data/map/railway_rw_allowlist.txt
"""

from __future__ import annotations

import argparse
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
DEFAULT_ALLOWLIST = ROOT / "data/map/railway_rw_allowlist.txt"
DEFAULT_ALIASES = ROOT / "data/map/railway_rw_aliases.csv"
DEFAULT_OUT = ROOT / "data/map/railways_voronoi.geojson"
DEFAULT_REPORT = ROOT / "data/map/railways_voronoi_report.json"
DEFAULT_BBOX = (19.0, 35.0, 180.0, 82.0)
DEFAULT_REGION = "ru,cis"


def parse_region_filter(raw: str) -> set[str] | None:
    """all → без фильтра; ru,cis → множество region_group."""
    s = (raw or "").strip().lower()
    if not s or s == "all":
        return None
    return {p.strip() for p in s.split(",") if p.strip()}


def load_rw_aliases(path: Path | None) -> dict[str, str]:
    """Синоним → канонический код (например БЖД → БЕЛ)."""
    if path is None or not path.is_file():
        return {}
    aliases: dict[str, str] = {}
    with path.open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if line.startswith("from,"):
                continue
            parts = [p.strip().upper() for p in line.split(",")]
            if len(parts) < 2 or not parts[0] or not parts[1]:
                continue
            aliases[parts[0]] = parts[1]
    return aliases


def apply_rw_aliases(
    assigned: list[dict],
    aliases: dict[str, str],
) -> tuple[list[dict], dict]:
    if not aliases:
        return assigned, {"aliases_file": None, "renamed_count": 0, "by_from": {}}

    renamed: Counter[str] = Counter()
    for p in assigned:
        rw = p["railway_rw"]
        canonical = aliases.get(rw)
        if canonical and canonical != rw:
            p["railway_rw_original"] = rw
            p["railway_rw"] = canonical
            renamed[rw] += 1

    by_from = {src: {"to": aliases[src], "count": cnt} for src, cnt in renamed.items()}
    return assigned, {
        "aliases_file": None,
        "renamed_count": sum(renamed.values()),
        "by_from": by_from,
    }


def load_allowlist(path: Path | None) -> set[str] | None:
    codes, _cis = load_allowlist_sections(path)
    return codes if codes else None


def load_allowlist_sections(path: Path | None) -> tuple[set[str], set[str]]:
    """Коды allowlist и подмножество СНГ (строки после «# СНГ» в файле)."""
    if path is None or not path.is_file():
        return set(), set()
    codes: set[str] = set()
    cis: set[str] = set()
    section_cis = False
    with path.open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            if line.startswith("#"):
                if "СНГ" in line.upper() or "сосед" in line.lower():
                    section_cis = True
                continue
            code = line.upper()
            codes.add(code)
            if section_cis:
                cis.add(code)
    return codes, cis


def _use_point_for_centroid(p: dict, rw: str, cis_rw: set[str]) -> bool:
    """Центроид СНГ-дорог — только станции region_group=cis (без «БЕЛ» по всей РФ)."""
    rg = p.get("region_group") or ""
    if rw in cis_rw:
        return rg == "cis"
    return rg == "ru"


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
        if rw_s in ("", "---"):
            rw_s = None
        out[esr6] = rw_s
    return out


def load_geo_points(
    geo_path: Path,
    *,
    region_groups: set[str] | None,
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
        rg = (region_group or "").strip()
        if region_groups is not None and rg not in region_groups:
            continue
        points.append(
            {
                "esr6": str(esr6).strip(),
                "lat": float(lat),
                "lon": float(lon),
                "region_group": rg,
                "country_hint": (country_hint or "").strip(),
            }
        )
    return points


def assign_railway_rw(
    points: list[dict],
    nsi_rw: dict[str, str | None],
    fallback: dict[str, str],
) -> tuple[list[dict], dict]:
    stats: Counter[str] = Counter()
    skipped: list[dict] = []
    assigned: list[dict] = []

    for p in points:
        esr6 = p["esr6"]
        rw = nsi_rw.get(esr6) if nsi_rw else None
        source = "nsi"
        rg = p.get("region_group") or ""
        country = (p.get("country_hint") or "").upper()
        use_csv = rg == "ru" or (not rg and country in ("", "RU"))

        if not rw and use_csv and len(esr6) >= 2:
            rw = fallback.get(esr6[:2])
            source = "csv_fallback" if rw else "none"
        if not rw:
            stats["no_rw"] += 1
            skipped.append({"esr6": esr6, "reason": "no_railway_rw", "region_group": rg})
            continue
        rw = rw.upper()
        if rw == "---":
            stats["no_rw"] += 1
            skipped.append({"esr6": esr6, "reason": "invalid_rw", "region_group": rg})
            continue
        stats[source] += 1
        assigned.append({**p, "railway_rw": rw, "rw_source": source})

    return assigned, {"stats": dict(stats), "skipped_sample": skipped[:100], "skipped_count": len(skipped)}


def filter_allowlist(
    assigned: list[dict],
    allowlist: set[str] | None,
) -> tuple[list[dict], dict]:
    if allowlist is None:
        return assigned, {"allowlist": None, "excluded_count": 0, "excluded_by_rw": {}}

    kept: list[dict] = []
    excluded: Counter[str] = Counter()
    for p in assigned:
        rw = p["railway_rw"]
        if rw in allowlist:
            kept.append(p)
        else:
            excluded[rw] += 1

    return kept, {
        "allowlist": sorted(allowlist),
        "excluded_count": sum(excluded.values()),
        "excluded_by_rw": dict(sorted(excluded.items(), key=lambda x: -x[1])),
        "excluded_sample": [
            {"esr6": p["esr6"], "railway_rw": p["railway_rw"], "region_group": p.get("region_group")}
            for p in assigned
            if p["railway_rw"] not in allowlist
        ][:50],
    }


def _dedupe_coords(
    points: list[dict],
) -> tuple[list[tuple[float, float]], list[int], int]:
    """Уникальные (lon, lat) для Voronoi; индекс rep-точки на каждую станцию."""
    rep_coords: list[tuple[float, float]] = []
    key_to_rep: dict[tuple[float, float], int] = {}
    point_to_rep: list[int] = []

    for p in points:
        key = (p["lon"], p["lat"])
        if key not in key_to_rep:
            key_to_rep[key] = len(rep_coords)
            rep_coords.append(key)
        point_to_rep.append(key_to_rep[key])

    duplicate_coord_stations = len(points) - len(rep_coords)
    return rep_coords, point_to_rep, duplicate_coord_stations


def _voronoi_cell_for_coord(
    coord: tuple[float, float],
    geoms: list,
    envelope,
) -> object | None:
    from shapely.geometry import Point

    pt = Point(coord)
    for g in geoms:
        if g.is_empty:
            continue
        if g.contains(pt) or g.boundary.distance(pt) < 1e-9:
            return g
    if not geoms:
        return None
    return min(geoms, key=lambda g: g.distance(pt))


def _railway_centroids(
    points: list[dict],
    cis_rw: set[str],
    *,
    min_filtered: int = 3,
) -> tuple[list[str], list[tuple[float, float]], Counter[str], dict[str, str]]:
    """
    Одна опорная точка на дорогу: среднее lon/lat станций.
    Для кодов СНГ — только станции с region_group=cis.
    """
    by_rw: dict[str, list[tuple[float, float]]] = defaultdict(list)
    by_rw_filtered: dict[str, list[tuple[float, float]]] = defaultdict(list)
    for p in points:
        rw = p["railway_rw"]
        coord = (p["lon"], p["lat"])
        by_rw[rw].append(coord)
        if _use_point_for_centroid(p, rw, cis_rw):
            by_rw_filtered[rw].append(coord)

    rw_order: list[str] = []
    sites: list[tuple[float, float]] = []
    station_counts: Counter[str] = Counter()
    centroid_source: dict[str, str] = {}

    for rw in sorted(by_rw.keys()):
        station_counts[rw] = len(by_rw[rw])
        filt = by_rw_filtered[rw]
        if len(filt) >= min_filtered:
            coords = filt
            centroid_source[rw] = "cis_filtered" if rw in cis_rw else "ru_filtered"
        else:
            coords = by_rw[rw]
            centroid_source[rw] = "all_stations_fallback"

        lon = sum(c[0] for c in coords) / len(coords)
        lat = sum(c[1] for c in coords) / len(coords)
        rw_order.append(rw)
        sites.append((lon, lat))

    return rw_order, sites, station_counts, centroid_source


def build_voronoi_geojson(
    points: list[dict],
    bbox: tuple[float, float, float, float],
    *,
    simplify_tol: float = 0.35,
    cis_rw: set[str] | None = None,
    mode: str = "centroids",
) -> tuple[dict, dict]:
    """
    centroids (по умолчанию): Voronoi между центроидами сетей — одна крупная зона на дорогу.
    stations: устаревший режим (union ячеек всех станций — «фракталы»).
    """
    if mode == "stations":
        return _build_voronoi_by_stations(points, bbox, simplify_tol=simplify_tol)

    from shapely.geometry import MultiPoint, box, mapping
    from shapely.ops import voronoi_diagram

    cis_rw = cis_rw or set()
    min_lon, min_lat, max_lon, max_lat = bbox
    envelope = box(min_lon, min_lat, max_lon, max_lat)

    rw_order, sites, station_counts, centroid_source = _railway_centroids(
        points, cis_rw
    )
    if len(sites) < 3:
        raise RuntimeError(f"Слишком мало дорог для Voronoi: {len(sites)}")

    mp = MultiPoint(sites)
    diagram = voronoi_diagram(mp, envelope=envelope)
    diagram_geoms = list(diagram.geoms)

    if len(diagram_geoms) != len(sites):
        raise RuntimeError(
            f"Voronoi centroids: {len(sites)} точек, {len(diagram_geoms)} ячеек"
        )

    features = []
    for idx, (rw, geom) in enumerate(zip(rw_order, diagram_geoms, strict=True)):
        if geom.is_empty:
            continue
        clipped = geom.intersection(envelope)
        if clipped.is_empty:
            continue
        if simplify_tol > 0:
            clipped = clipped.simplify(simplify_tol, preserve_topology=True)
        minx, miny, maxx, maxy = clipped.bounds
        c_lon, c_lat = sites[idx]
        features.append(
            {
                "type": "Feature",
                "properties": {
                    "rw": rw,
                    "station_count": station_counts[rw],
                    "centroid_lon": c_lon,
                    "centroid_lat": c_lat,
                    "centroid_source": centroid_source.get(rw, ""),
                    "label_lon": minx,
                    "label_lat": maxy,
                },
                "geometry": mapping(clipped),
            }
        )

    collection = {"type": "FeatureCollection", "features": features}
    meta = {
        "mode": "centroids",
        "zone_count": len(features),
        "station_count": len(points),
        "railway_count": len(rw_order),
        "centroid_source": centroid_source,
        "railways": dict(sorted(station_counts.items())),
    }
    return collection, meta


def _build_voronoi_by_stations(
    points: list[dict],
    bbox: tuple[float, float, float, float],
    *,
    simplify_tol: float = 0.08,
) -> tuple[dict, dict]:
    """Старый режим: union Voronoi-ячеек каждой станции (даёт фрактальные MultiPolygon)."""
    from shapely.geometry import MultiPoint, box, mapping
    from shapely.ops import unary_union, voronoi_diagram

    min_lon, min_lat, max_lon, max_lat = bbox
    envelope = box(min_lon, min_lat, max_lon, max_lat)

    rep_coords, point_to_rep, duplicate_coord_stations = _dedupe_coords(points)
    mp = MultiPoint(rep_coords)
    diagram = voronoi_diagram(mp, envelope=envelope)
    diagram_geoms = list(diagram.geoms)

    if len(diagram_geoms) == len(rep_coords):
        rep_geoms = diagram_geoms
    elif len(diagram_geoms) < len(rep_coords):
        rep_geoms = [
            _voronoi_cell_for_coord(c, diagram_geoms, envelope) for c in rep_coords
        ]
        if any(g is None for g in rep_geoms):
            raise RuntimeError(
                f"Voronoi: {len(rep_coords)} уникальных координат, "
                f"{len(diagram_geoms)} ячеек — не удалось сопоставить все точки"
            )
    else:
        raise RuntimeError(
            f"Voronoi: неожиданно {len(diagram_geoms)} ячеек при {len(rep_coords)} точках"
        )

    by_rw: dict[str, list] = defaultdict(list)
    rw_station_count: Counter[str] = Counter()

    for i, p in enumerate(points):
        geom = rep_geoms[point_to_rep[i]]
        if geom is None or geom.is_empty:
            continue
        rw = p["railway_rw"]
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
        "mode": "stations",
        "zone_count": len(features),
        "station_count": len(points),
        "unique_coord_count": len(rep_coords),
        "duplicate_coord_stations": duplicate_coord_stations,
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
        default=DEFAULT_REGION,
        help="фильтр region_group: all | ru | cis | ru,cis (по умолчанию ru,cis)",
    )
    parser.add_argument(
        "--allowlist",
        type=Path,
        default=DEFAULT_ALLOWLIST,
        help="файл допустимых кодов railway_rw (пустой путь + --no-allowlist)",
    )
    parser.add_argument(
        "--no-allowlist",
        action="store_true",
        help="не фильтровать по allowlist (все коды из NSI)",
    )
    parser.add_argument(
        "--aliases",
        type=Path,
        default=DEFAULT_ALIASES,
        help="CSV синонимов railway_rw (from,to); пустой путь отключает",
    )
    parser.add_argument(
        "--no-aliases",
        action="store_true",
        help="не применять railway_rw_aliases.csv",
    )
    parser.add_argument(
        "--bbox",
        default=",".join(str(x) for x in DEFAULT_BBOX),
        help="min_lon,min_lat,max_lon,max_lat",
    )
    parser.add_argument(
        "--mode",
        choices=("centroids", "stations"),
        default="centroids",
        help="centroids — одна зона на дорогу (рекомендуется); stations — union по всем станциям",
    )
    parser.add_argument(
        "--simplify",
        type=float,
        default=None,
        help="tolerance simplify (градусы); по умолчанию 0.35 для centroids, 0.08 для stations",
    )
    args = parser.parse_args()

    bbox_parts = [float(x) for x in args.bbox.split(",")]
    if len(bbox_parts) != 4:
        print("bbox: нужно 4 числа", file=sys.stderr)
        return 1
    bbox = tuple(bbox_parts)  # type: ignore[assignment]

    if not args.geo.is_file():
        print(f"Нет файла: {args.geo}", file=sys.stderr)
        return 1

    region_groups = parse_region_filter(args.region)
    _allow, cis_rw = load_allowlist_sections(
        None if args.no_allowlist else args.allowlist
    )
    allowlist = None if args.no_allowlist else (_allow or None)
    if allowlist is not None and not allowlist:
        print(f"WARN: пустой allowlist {args.allowlist}", file=sys.stderr)

    simplify = args.simplify
    if simplify is None:
        simplify = 0.35 if args.mode == "centroids" else 0.08

    fallback = load_esr_fallback(args.fallback) if args.fallback.is_file() else {}
    nsi_rw = load_nsi_rw(args.nsi)
    aliases = (
        {}
        if args.no_aliases
        else load_rw_aliases(args.aliases if str(args.aliases) else None)
    )
    geo_points = load_geo_points(args.geo, region_groups=region_groups)
    assigned, assign_report = assign_railway_rw(geo_points, nsi_rw, fallback)
    assigned, alias_report = apply_rw_aliases(assigned, aliases)
    if not args.no_aliases and args.aliases.is_file():
        alias_report["aliases_file"] = str(args.aliases)
    filtered, filter_report = filter_allowlist(assigned, allowlist)

    if len(filtered) < 3:
        print(
            f"Слишком мало станций после фильтров: {len(filtered)} "
            f"(geo={len(geo_points)}, assigned={len(assigned)})",
            file=sys.stderr,
        )
        return 1

    try:
        collection, voronoi_meta = build_voronoi_geojson(
            filtered,
            bbox,
            simplify_tol=simplify,
            cis_rw=cis_rw,
            mode=args.mode,
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

    by_region = Counter(p.get("region_group", "") for p in filtered)
    report = {
        "built_at": datetime.now(timezone.utc).isoformat(),
        "voronoi_mode": args.mode,
        "simplify_tol": simplify,
        "cis_rw_codes": sorted(cis_rw) if cis_rw else [],
        "region_filter": args.region,
        "region_groups_resolved": sorted(region_groups) if region_groups else "all",
        "bbox": list(bbox),
        "inputs": {
            "geo": str(args.geo),
            "nsi": str(args.nsi) if args.nsi.is_file() else None,
            "fallback": str(args.fallback) if args.fallback.is_file() else None,
            "allowlist": None if args.no_allowlist else str(args.allowlist),
            "aliases": None if args.no_aliases else str(args.aliases),
        },
        "counts": {
            "geo_points": len(geo_points),
            "assigned": len(assigned),
            "after_allowlist": len(filtered),
            "by_region_group": dict(sorted(by_region.items())),
        },
        "output": str(args.out),
        "assignment": assign_report,
        "rw_aliases": alias_report,
        "allowlist_filter": filter_report,
        "voronoi": voronoi_meta,
        "disclaimer": "Условные границы (Voronoi по станциям), не официальные границы РЖД",
    }
    with args.report.open("w", encoding="utf-8") as f:
        json.dump(report, f, ensure_ascii=False, indent=2)

    excl = filter_report.get("excluded_count", 0)
    print(
        f"OK: {args.out} ({len(collection['features'])} zones, "
        f"{len(filtered)} stations, excluded={excl})"
    )
    print(f"report: {args.report}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

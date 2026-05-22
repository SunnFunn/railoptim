"""Join NSI parquet + OSM esr index → SQLite + отчёты."""

from __future__ import annotations

import csv
import sqlite3
from collections import Counter
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def load_parquet_rows(path: Path) -> list[dict[str, Any]]:
    try:
        import pyarrow.parquet as pq
    except ImportError as e:
        raise SystemExit(
            "geo_join: нужен pyarrow (scripts/stations/requirements-stations.txt)"
        ) from e
    if not path.is_file():
        raise SystemExit(f"geo_join: файл не найден: {path}")
    return pq.read_table(path).to_pylist()


def is_valid_coord(lat: Any, lon: Any) -> bool:
    try:
        la, lo = float(lat), float(lon)
    except (TypeError, ValueError):
        return False
    return abs(la) <= 90.0 and abs(lo) <= 180.0


@dataclass
class GeoStationRow:
    esr6: str
    name: str
    lat: float
    lon: float
    country_hint: str
    region_group: str
    source: str
    match_method: str
    osm_id: int | None
    name_osm: str
    confidence: float
    built_at: str

    def as_sql_tuple(self) -> tuple[Any, ...]:
        return (
            self.esr6,
            self.name,
            self.lat,
            self.lon,
            self.country_hint,
            self.region_group,
            self.source,
            self.match_method,
            self.osm_id,
            self.name_osm,
            self.confidence,
            self.built_at,
        )


@dataclass
class JoinResult:
    rows: list[GeoStationRow] = field(default_factory=list)
    unmatched: list[dict[str, str]] = field(default_factory=list)
    cross_border: list[dict[str, Any]] = field(default_factory=list)
    invalid_coords: list[dict[str, str]] = field(default_factory=list)


def _index_coord_rows(rows: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    out: dict[str, dict[str, Any]] = {}
    for row in rows:
        esr6 = str(row.get("esr6", "")).strip()
        if len(esr6) == 6 and esr6.isdigit():
            out[esr6] = row
    return out


def _row_from_coord(
    nsi: dict[str, Any],
    coord: dict[str, Any],
    *,
    built_at: str,
) -> tuple[GeoStationRow | None, dict[str, Any] | None]:
    """Собрать GeoStationRow или вернуть причину unmatched/invalid."""
    esr6 = str(nsi.get("esr6", "")).strip()
    name = str(nsi.get("name_nsi", "")).strip()
    country_hint = str(nsi.get("country_hint", ""))
    region_group = str(nsi.get("region_group", "unknown"))

    lat, lon = coord.get("lat"), coord.get("lon")
    if not is_valid_coord(lat, lon):
        return None, {
            "kind": "invalid_coords",
            "esr6": esr6,
            "name_nsi": name,
            "lat": str(lat),
            "lon": str(lon),
            "region_group": region_group,
        }

    confidence = float(coord.get("confidence", 1.0) or 1.0)
    cross_border: dict[str, Any] | None = None
    osm_region = str(coord.get("region_group", ""))
    if osm_region and region_group and osm_region != region_group:
        cross_border = {
            "esr6": esr6,
            "name_nsi": name,
            "nsi_region_group": region_group,
            "osm_region_group": osm_region,
            "pbf_region": str(coord.get("pbf_region", "")),
            "country_hint": country_hint,
        }
        confidence = min(confidence, 0.8)

    osm_id_raw = coord.get("osm_id")
    osm_id: int | None
    try:
        osm_id = int(osm_id_raw) if osm_id_raw is not None else None
    except (TypeError, ValueError):
        osm_id = None

    row = GeoStationRow(
        esr6=esr6,
        name=name,
        lat=float(lat),
        lon=float(lon),
        country_hint=country_hint,
        region_group=region_group,
        source=str(coord.get("source", "osm_pbf")),
        match_method=str(coord.get("match_method", coord.get("tag_name", ""))),
        osm_id=osm_id,
        name_osm=str(coord.get("name_osm", "")),
        confidence=confidence,
        built_at=built_at,
    )
    return row, cross_border


def join_nsi_osm(
    nsi_rows: list[dict[str, Any]],
    osm_rows: list[dict[str, Any]],
    sbin_rows: list[dict[str, Any]] | None = None,
    *,
    built_at: str | None = None,
) -> JoinResult:
    """Join NSI с координатами: Tier1 osm_pbf, Tier2 osm_sbin (fallback)."""
    built_at = built_at or datetime.now(timezone.utc).isoformat()
    osm_by_esr = _index_coord_rows(osm_rows)
    sbin_by_esr = _index_coord_rows(sbin_rows or [])

    result = JoinResult()
    for nsi in nsi_rows:
        esr6 = str(nsi.get("esr6", "")).strip()
        name = str(nsi.get("name_nsi", "")).strip()
        region_group = str(nsi.get("region_group", "unknown"))

        coord = osm_by_esr.get(esr6)
        if coord is None:
            coord = sbin_by_esr.get(esr6)

        if coord is None:
            result.unmatched.append(
                {"esr6": esr6, "name_nsi": name, "region_group": region_group}
            )
            continue

        row, extra = _row_from_coord(nsi, coord, built_at=built_at)
        if row is None:
            assert extra is not None
            if extra["kind"] == "invalid_coords":
                result.invalid_coords.append(
                    {
                        "esr6": extra["esr6"],
                        "name_nsi": extra["name_nsi"],
                        "lat": extra["lat"],
                        "lon": extra["lon"],
                    }
                )
            result.unmatched.append(
                {"esr6": esr6, "name_nsi": name, "region_group": region_group}
            )
            continue

        if extra is not None:
            result.cross_border.append(extra)

        result.rows.append(row)

    return result


def write_sqlite(rows: list[GeoStationRow], path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.is_file():
        path.unlink()
    conn = sqlite3.connect(path)
    try:
        conn.execute(
            """
            CREATE TABLE stations_geo (
              esr6         TEXT PRIMARY KEY NOT NULL,
              name         TEXT NOT NULL,
              lat          REAL NOT NULL,
              lon          REAL NOT NULL,
              country_hint TEXT,
              region_group TEXT,
              source       TEXT NOT NULL,
              match_method TEXT NOT NULL,
              osm_id       INTEGER,
              name_osm     TEXT,
              confidence   REAL NOT NULL DEFAULT 1.0,
              built_at     TEXT NOT NULL
            )
            """
        )
        conn.execute("CREATE INDEX idx_stations_geo_esr6 ON stations_geo(esr6)")
        conn.executemany(
            """
            INSERT INTO stations_geo (
              esr6, name, lat, lon, country_hint, region_group,
              source, match_method, osm_id, name_osm, confidence, built_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            [r.as_sql_tuple() for r in rows],
        )
        conn.commit()
    finally:
        conn.close()


def write_unmatched_csv(rows: list[dict[str, str]], path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="") as f:
        w = csv.DictWriter(f, fieldnames=["esr6", "name_nsi", "region_group"])
        w.writeheader()
        w.writerows(rows)


def write_cross_border_csv(rows: list[dict[str, Any]], path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fields = [
        "esr6",
        "name_nsi",
        "nsi_region_group",
        "osm_region_group",
        "pbf_region",
        "country_hint",
    ]
    with path.open("w", encoding="utf-8", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        for row in rows:
            w.writerow({k: row.get(k, "") for k in fields})


def coverage_by_region_group(
    nsi_rows: list[dict[str, Any]],
    matched_esr6: set[str],
) -> dict[str, dict[str, float | int]]:
    totals: Counter[str] = Counter()
    matched: Counter[str] = Counter()
    for row in nsi_rows:
        rg = str(row.get("region_group", "unknown"))
        esr6 = str(row.get("esr6", ""))
        totals[rg] += 1
        if esr6 in matched_esr6:
            matched[rg] += 1

    out: dict[str, dict[str, float | int]] = {}
    for rg in sorted(totals.keys()):
        t = totals[rg]
        m = matched[rg]
        pct = round(100.0 * m / t, 2) if t else 0.0
        out[rg] = {"total": t, "matched": m, "coverage_pct": pct}
    return out


def build_report(
    join: JoinResult,
    nsi_rows: list[dict[str, Any]],
    osm_rows: list[dict[str, Any]],
    sbin_rows: list[dict[str, Any]] | None = None,
    *,
    built_at: str,
    paths: dict[str, str],
) -> dict[str, Any]:
    nsi_unique = len(nsi_rows)
    osm_unique = len({str(r.get("esr6")) for r in osm_rows if r.get("esr6")})
    sbin_list = sbin_rows or []
    sbin_unique = len({str(r.get("esr6")) for r in sbin_list if r.get("esr6")})
    matched_esr = {r.esr6 for r in join.rows}
    matched_n = len(join.rows)
    coverage_pct = round(100.0 * matched_n / nsi_unique, 2) if nsi_unique else 0.0

    by_source = Counter(r.source for r in join.rows)
    by_method = Counter(r.match_method for r in join.rows)
    by_pbf: Counter[str] = Counter()
    osm_by_esr = {str(r["esr6"]): r for r in osm_rows if r.get("esr6")}
    for esr6 in matched_esr:
        if esr6 in osm_by_esr:
            pbf = str(osm_by_esr[esr6].get("pbf_region", "unknown"))
            by_pbf[pbf] += 1

    matched_via_sbin = sum(1 for r in join.rows if r.source == "osm_sbin")
    matched_via_pbf = sum(1 for r in join.rows if r.source == "osm_pbf")

    return {
        "built_at": built_at,
        "nsi_total": nsi_unique,
        "nsi_unique_esr6": nsi_unique,
        "osm_index_size": len(osm_rows),
        "osm_unique_esr6": osm_unique,
        "sbin_index_size": len(sbin_list),
        "sbin_unique_esr6": sbin_unique,
        "matched_with_coords": matched_n,
        "matched_via_osm_pbf": matched_via_pbf,
        "matched_via_osm_sbin": matched_via_sbin,
        "coverage_pct": coverage_pct,
        "coverage_by_region_group": coverage_by_region_group(nsi_rows, matched_esr),
        "unmatched_count": len(join.unmatched),
        "invalid_coords_count": len(join.invalid_coords),
        "cross_border_count": len(join.cross_border),
        "by_source": dict(sorted(by_source.items())),
        "by_match_method": dict(sorted(by_method.items())),
        "by_pbf_region": dict(sorted(by_pbf.items())),
        **paths,
    }

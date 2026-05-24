"""Tier 0: ручные координаты из manual_coords.csv."""

from __future__ import annotations

import csv
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from stations_etl.normalize import normalize_esr6
from stations_etl.paths import MANUAL_COORDS_CSV


@dataclass(frozen=True)
class ManualCoord:
    esr6: str
    lat: float
    lon: float
    note: str = ""


def load_manual_coords(path: Path | None = None) -> dict[str, dict[str, Any]]:
    """Загрузить manual coords → dict esr6 → row для join (как OSM/sbin parquet row)."""
    path = path or MANUAL_COORDS_CSV
    if not path.is_file():
        return {}

    out: dict[str, dict[str, Any]] = {}
    with path.open(encoding="utf-8", newline="") as f:
        for row in csv.reader(f):
            if not row or row[0].startswith("#"):
                continue
            if row[0].strip().lower() == "esr6":
                continue
            if len(row) < 3:
                continue

            esr6 = normalize_esr6(row[0].strip())
            if len(esr6) != 6 or not esr6.isdigit():
                continue
            try:
                lat = float(row[1].strip())
                lon = float(row[2].strip())
            except ValueError:
                continue
            note = row[3].strip() if len(row) > 3 else ""

            out[esr6] = {
                "esr6": esr6,
                "lat": lat,
                "lon": lon,
                "source": "manual",
                "match_method": "manual_csv",
                "tag_name": "manual_csv",
                "osm_id": None,
                "name_osm": note,
                "confidence": 1.0,
            }
    return out


def load_manual_coord_records(path: Path | None = None) -> list[ManualCoord]:
    """Список записей (для отчётов / тестов)."""
    rows = load_manual_coords(path)
    return [
        ManualCoord(
            esr6=esr6,
            lat=float(r["lat"]),
            lon=float(r["lon"]),
            note=str(r.get("name_osm", "")),
        )
        for esr6, r in sorted(rows.items())
    ]

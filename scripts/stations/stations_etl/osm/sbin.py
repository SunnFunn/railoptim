"""Tier 2: osm.sbin.ru/osm2esr.csv → esr6 index (fallback для РФ и др.)."""

from __future__ import annotations

import csv
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from io import StringIO
from pathlib import Path
from typing import Any

from stations_etl.normalize import normalize_esr6
from stations_etl.paths import SBIN_CACHE_CSV, SBIN_INDEX_URL

RAILWAY_PRIORITY = {"station": 30, "halt": 20, "stop": 10}
SOURCE = "osm_sbin"
MATCH_METHOD = "osm2esr_csv"


@dataclass
class SbinEsrCandidate:
    esr6: str
    lat: float
    lon: float
    osm_id: int
    name_osm: str
    railway: str
    sbin_status: int
    osm_type: int

    @property
    def railway_priority(self) -> int:
        return RAILWAY_PRIORITY.get(self.railway, 0)

    @property
    def confidence(self) -> float:
        if self.sbin_status == 1:
            return 0.95
        return 0.75

    def as_dict(self) -> dict[str, Any]:
        return {
            "esr6": self.esr6,
            "lat": self.lat,
            "lon": self.lon,
            "osm_type": "node" if self.osm_type == 0 else "way",
            "osm_id": self.osm_id,
            "tag_name": "esr",
            "match_method": MATCH_METHOD,
            "name_osm": self.name_osm,
            "pbf_region": "sbin",
            "region_group": "",
            "railway": self.railway,
            "confidence": self.confidence,
            "source": SOURCE,
            "sbin_status": self.sbin_status,
        }


def download_osm2esr_csv(
    dest: Path | None = None,
    *,
    url: str = SBIN_INDEX_URL,
    timeout: float = 120.0,
    force: bool = False,
) -> Path:
    dest = dest or SBIN_CACHE_CSV
    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.is_file() and not force:
        if dest.stat().st_size > 1024:
            print(f"  skip sbin download: {dest.name} уже есть", flush=True)
            return dest

    print(f"  download sbin: {url}", flush=True)
    req = urllib.request.Request(url, headers={"User-Agent": "railoptim-stations-etl/1.0"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            data = resp.read()
    except urllib.error.HTTPError as e:
        raise SystemExit(f"download sbin failed: HTTP {e.code} {url}") from e
    except urllib.error.URLError as e:
        raise SystemExit(f"download sbin failed: {e.reason}") from e

    if len(data) < 1024:
        raise SystemExit(f"download sbin: подозрительно малый ответ ({len(data)} bytes)")

    dest.write_bytes(data)
    print(f"  saved {dest} ({len(data) // 1024} KiB)", flush=True)
    return dest


def parse_osm2esr_csv(text: str) -> list[SbinEsrCandidate]:
    reader = csv.reader(StringIO(text), delimiter=";", quotechar='"')
    header = next(reader, None)
    if not header or header[0].strip('"').lower() != "esr":
        raise SystemExit("sbin csv: ожидается заголовок с колонкой esr")

    out: list[SbinEsrCandidate] = []
    for row in reader:
        if len(row) < 12:
            continue
        esr6 = normalize_esr6(row[0])
        if len(esr6) != 6 or not esr6.isdigit():
            continue
        try:
            sbin_status = int(row[1])
            osm_type = int(row[2])
            osm_id = int(row[3])
            lat = float(row[4])
            lon = float(row[5])
        except (TypeError, ValueError):
            continue
        if abs(lat) > 90 or abs(lon) > 180:
            continue
        name = row[6].strip()
        railway = row[10].strip() or "station"
        out.append(
            SbinEsrCandidate(
                esr6=esr6,
                lat=lat,
                lon=lon,
                osm_id=osm_id,
                name_osm=name,
                railway=railway,
                sbin_status=sbin_status,
                osm_type=osm_type,
            )
        )
    return out


def merge_sbin_candidates(candidates: list[SbinEsrCandidate]) -> dict[str, SbinEsrCandidate]:
    """Merge по esr6: status=1 > status=2, затем railway priority."""
    by_esr: dict[str, list[SbinEsrCandidate]] = {}
    for c in candidates:
        by_esr.setdefault(c.esr6, []).append(c)

    index: dict[str, SbinEsrCandidate] = {}
    for esr6, group in by_esr.items():
        status1 = [c for c in group if c.sbin_status == 1]
        pool = status1 if status1 else group
        best = max(pool, key=lambda c: (c.sbin_status == 1, c.railway_priority, -c.osm_id))
        index[esr6] = best
    return index


def load_sbin_index_from_csv(path: Path) -> list[dict[str, Any]]:
    if not path.is_file():
        raise SystemExit(f"sbin: файл не найден: {path}")
    text = path.read_text(encoding="utf-8")
    candidates = parse_osm2esr_csv(text)
    merged = merge_sbin_candidates(candidates)
    return [c.as_dict() for c in sorted(merged.values(), key=lambda x: x.esr6)]


def build_sbin_index_rows(
    csv_path: Path | None = None,
    *,
    download: bool = False,
    force_download: bool = False,
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    path = csv_path or SBIN_CACHE_CSV
    if download or not path.is_file():
        download_osm2esr_csv(path, force=force_download)

    text = path.read_text(encoding="utf-8")
    candidates = parse_osm2esr_csv(text)
    merged = merge_sbin_candidates(candidates)
    rows = [c.as_dict() for c in sorted(merged.values(), key=lambda x: x.esr6)]

    status1 = sum(1 for c in merged.values() if c.sbin_status == 1)
    status2 = len(merged) - status1
    by_railway: dict[str, int] = {}
    for c in merged.values():
        by_railway[c.railway] = by_railway.get(c.railway, 0) + 1

    report: dict[str, Any] = {
        "built_at": datetime.now(timezone.utc).isoformat(),
        "source_url": SBIN_INDEX_URL,
        "csv_path": str(path),
        "candidates_total": len(candidates),
        "sbin_unique_esr6": len(rows),
        "status1_count": status1,
        "status2_count": status2,
        "by_railway": dict(sorted(by_railway.items())),
    }
    return rows, report

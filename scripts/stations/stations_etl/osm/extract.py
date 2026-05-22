"""Извлечение esr6 из OSM PBF и merge в индекс."""

from __future__ import annotations

import math
import re
from dataclasses import dataclass, field
from typing import Any, Iterator

from stations_etl.normalize import normalize_esr6
from stations_etl.osm.geofabrik import GeofabrikRegion

_SPLIT_RE = re.compile(r"[;,]")

RAILWAY_VALUES = frozenset({"station", "halt", "stop"})
RAILWAY_PRIORITY = {"station": 30, "halt": 20, "stop": 10}

TAG_PRIORITY = {
    "ref": 1,
    "uic_ref": 2,
    "esr:user": 3,
    "railway:ref": 4,
}


@dataclass
class OsmEsrCandidate:
    esr6: str
    lat: float
    lon: float
    osm_type: str
    osm_id: int
    tag_name: str
    name_osm: str
    pbf_region: str
    pbf_priority: int
    region_group: str
    railway: str
    match_method: str
    confidence: float = 1.0

    @property
    def railway_priority(self) -> int:
        return RAILWAY_PRIORITY.get(self.railway, 0)

    @property
    def tag_priority(self) -> int:
        return TAG_PRIORITY.get(self.tag_name, 99)

    def sort_key(self) -> tuple[int, int, int]:
        return (self.pbf_priority, self.railway_priority, -self.tag_priority)

    def as_dict(self) -> dict[str, Any]:
        return {
            "esr6": self.esr6,
            "lat": self.lat,
            "lon": self.lon,
            "osm_type": self.osm_type,
            "osm_id": self.osm_id,
            "tag_name": self.tag_name,
            "match_method": self.match_method,
            "name_osm": self.name_osm,
            "pbf_region": self.pbf_region,
            "region_group": self.region_group,
            "railway": self.railway,
            "confidence": self.confidence,
            "source": "osm_pbf",
        }


def iter_esr_from_tag_value(tag_name: str, raw: str) -> Iterator[str]:
    for part in _SPLIT_RE.split(raw):
        code = normalize_esr6(part.strip())
        if len(code) == 6 and code.isdigit():
            yield code


def iter_esr_from_tags(tags: Any) -> Iterator[tuple[str, str]]:
    """(tag_name, esr6) в порядке приоритета тегов."""
    for tag_name in TAG_PRIORITY:
        val = tags.get(tag_name)
        if not val:
            continue
        for esr6 in iter_esr_from_tag_value(tag_name, str(val)):
            yield tag_name, esr6


def in_bbox(lat: float, lon: float, bbox: tuple[float, float, float, float] | None) -> bool:
    if bbox is None:
        return True
    lon_min, lat_min, lon_max, lat_max = bbox
    return lat_min <= lat <= lat_max and lon_min <= lon <= lon_max


def haversine_m(lat1: float, lon1: float, lat2: float, lon2: float) -> float:
    r = 6_371_000.0
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dphi = math.radians(lat2 - lat1)
    dlmb = math.radians(lon2 - lon1)
    a = math.sin(dphi / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dlmb / 2) ** 2
    return 2 * r * math.asin(min(1.0, math.sqrt(a)))


def _name_from_tags(tags: Any) -> str:
    for key in ("name", "name:ru", "name:en"):
        v = tags.get(key)
        if v:
            return str(v).strip()
    return ""


def extract_from_pbf(
    pbf_path: str | Any,
    region: GeofabrikRegion,
) -> list[OsmEsrCandidate]:
    try:
        import osmium
    except ImportError as e:
        raise SystemExit(
            "osm extract: установите osmium (pip) и libosmium (brew/apt)\n"
            "  pip install -r scripts/stations/requirements-stations.txt"
        ) from e

    out: list[OsmEsrCandidate] = []

    class Handler(osmium.SimpleHandler):
        def __init__(self) -> None:
            super().__init__()
            self.node_locs = osmium.index.create_map("sparse_mem_array")

        def node(self, n: osmium.osm.Node) -> None:
            if n.location.valid():
                self.node_locs.set(n.id, n.location)
            rw = n.tags.get("railway")
            if rw not in RAILWAY_VALUES:
                return
            lat, lon = n.location.lat, n.location.lon
            if not n.location.valid() or not in_bbox(lat, lon, region.bbox):
                return
            self._emit("node", int(n.id), n.tags, lat, lon, str(rw))

        def way(self, w: osmium.osm.Way) -> None:
            rw = w.tags.get("railway")
            if rw not in RAILWAY_VALUES:
                return
            lats: list[float] = []
            lons: list[float] = []
            for nr in w.nodes:
                loc = self.node_locs.get(nr.ref)
                if loc is None or not loc.valid():
                    continue
                lats.append(loc.lat)
                lons.append(loc.lon)
            if not lats:
                return
            lat = sum(lats) / len(lats)
            lon = sum(lons) / len(lons)
            if not in_bbox(lat, lon, region.bbox):
                return
            self._emit("way", int(w.id), w.tags, lat, lon, str(rw))

        def _emit(
            self,
            osm_type: str,
            osm_id: int,
            tags: Any,
            lat: float,
            lon: float,
            railway: str,
        ) -> None:
            name = _name_from_tags(tags)
            seen_esr: set[str] = set()
            for tag_name, esr6 in iter_esr_from_tags(tags):
                if esr6 in seen_esr:
                    continue
                seen_esr.add(esr6)
                out.append(
                    OsmEsrCandidate(
                        esr6=esr6,
                        lat=lat,
                        lon=lon,
                        osm_type=osm_type,
                        osm_id=osm_id,
                        tag_name=tag_name,
                        name_osm=name,
                        pbf_region=region.id,
                        pbf_priority=region.priority,
                        region_group=region.region_group,
                        railway=railway,
                        match_method=tag_name,
                    )
                )

    handler = Handler()
    handler.apply_file(str(pbf_path), locations=True)
    return out


@dataclass
class MergeResult:
    index: dict[str, OsmEsrCandidate] = field(default_factory=dict)
    ambiguous: list[dict[str, Any]] = field(default_factory=list)
    cross_border: list[dict[str, Any]] = field(default_factory=list)
    candidates_total: int = 0


def merge_candidates(candidates: list[OsmEsrCandidate]) -> MergeResult:
    """Merge по esr6: выше priority PBF, railway, tag; конфликты координат в отчёт."""
    by_esr: dict[str, list[OsmEsrCandidate]] = {}
    for c in candidates:
        by_esr.setdefault(c.esr6, []).append(c)

    result = MergeResult(candidates_total=len(candidates))
    dist_ambiguous_m = 500.0
    dist_cross_border_m = 1000.0

    for esr6, group in sorted(by_esr.items()):
        best = max(group, key=lambda c: c.sort_key())
        regions = {c.pbf_region for c in group}
        coords = {(round(c.lat, 5), round(c.lon, 5)) for c in group}

        if len(group) > 1:
            max_dist = 0.0
            for i, a in enumerate(group):
                for b in group[i + 1 :]:
                    max_dist = max(max_dist, haversine_m(a.lat, a.lon, b.lat, b.lon))
            if len(coords) > 1:
                entry = {
                    "esr6": esr6,
                    "candidates": len(group),
                    "max_distance_m": round(max_dist, 1),
                    "pbf_regions": sorted(regions),
                    "chosen_osm_id": best.osm_id,
                    "chosen_pbf_region": best.pbf_region,
                }
                result.ambiguous.append(entry)
                if max_dist > dist_ambiguous_m:
                    best.confidence = 0.8

            if len(regions) > 1 and max_dist > dist_cross_border_m:
                result.cross_border.append(
                    {
                        "esr6": esr6,
                        "pbf_regions": sorted(regions),
                        "max_distance_m": round(max_dist, 1),
                        "chosen_pbf_region": best.pbf_region,
                    }
                )

        result.index[esr6] = best

    return result

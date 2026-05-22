"""Geofabrik manifest (geofabrik_regions.yaml) и загрузка PBF."""

from __future__ import annotations

import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from stations_etl.paths import GEOFABRIK_MANIFEST, PBF_CACHE_DIR, REPO_ROOT


@dataclass(frozen=True)
class GeofabrikRegion:
    id: str
    geofabrik_slug: str
    region_group: str
    priority: int
    country_iso: str | None = None
    required: bool = False
    optional: bool = False
    bbox: tuple[float, float, float, float] | None = None
    note: str = ""


@dataclass
class GeofabrikManifest:
    version: int
    cache_dir: Path
    base_url: str
    regions: list[GeofabrikRegion]

    def sorted_regions(self) -> list[GeofabrikRegion]:
        return sorted(self.regions, key=lambda r: r.priority)


def _parse_bbox(raw: Any) -> tuple[float, float, float, float] | None:
    if raw is None:
        return None
    if not isinstance(raw, (list, tuple)) or len(raw) != 4:
        raise ValueError(f"bbox must be 4 floats, got {raw!r}")
    return float(raw[0]), float(raw[1]), float(raw[2]), float(raw[3])


def load_manifest(path: Path | None = None) -> GeofabrikManifest:
    path = path or GEOFABRIK_MANIFEST
    try:
        import yaml
    except ImportError as e:
        raise SystemExit(
            "geofabrik: нужен pyyaml (scripts/stations/requirements-stations.txt)"
        ) from e

    data = yaml.safe_load(path.read_text(encoding="utf-8"))
    cache_rel = data.get("cache_dir", "data/stations/cache/pbf")
    cache_dir = Path(cache_rel)
    if not cache_dir.is_absolute():
        cache_dir = REPO_ROOT / cache_dir

    regions: list[GeofabrikRegion] = []
    for item in data.get("regions", []):
        regions.append(
            GeofabrikRegion(
                id=str(item["id"]),
                geofabrik_slug=str(item["geofabrik_slug"]),
                region_group=str(item.get("region_group", "unknown")),
                priority=int(item.get("priority", 20)),
                country_iso=item.get("country_iso"),
                required=bool(item.get("required", False)),
                optional=bool(item.get("optional", False)),
                bbox=_parse_bbox(item.get("bbox")),
                note=str(item.get("note", "")),
            )
        )

    return GeofabrikManifest(
        version=int(data.get("version", 1)),
        cache_dir=cache_dir,
        base_url=str(data.get("base_url", "https://download.geofabrik.de")).rstrip("/"),
        regions=regions,
    )


def pbf_path(manifest: GeofabrikManifest, region: GeofabrikRegion) -> Path:
    name = region.geofabrik_slug.split("/")[-1]
    return manifest.cache_dir / f"{name}.osm.pbf"


def pbf_url(manifest: GeofabrikManifest, region: GeofabrikRegion) -> str:
    return f"{manifest.base_url}/{region.geofabrik_slug}.osm.pbf"


def download_pbf(
    manifest: GeofabrikManifest,
    region: GeofabrikRegion,
    *,
    force: bool = False,
    timeout: float = 3600.0,
) -> Path:
    dest = pbf_path(manifest, region)
    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.is_file() and not force:
        if dest.stat().st_size > 1024 * 1024:
            print(f"  skip {region.id}: {dest.name} уже есть", flush=True)
            return dest
        print(f"  replace {region.id}: подозрительно малый {dest.name}", flush=True)

    url = pbf_url(manifest, region)
    tmp = dest.with_suffix(".osm.pbf.part")
    if tmp.is_file():
        tmp.unlink()
    print(f"  download {region.id}: {url}", flush=True)

    req = urllib.request.Request(url, headers={"User-Agent": "railoptim-stations-etl/1.0"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            total = int(resp.headers.get("Content-Length", 0) or 0)
            done = 0
            chunk_size = 1024 * 1024
            with tmp.open("wb") as out:
                while True:
                    chunk = resp.read(chunk_size)
                    if not chunk:
                        break
                    out.write(chunk)
                    done += len(chunk)
                    if total > 0 and done % (50 * chunk_size) < chunk_size:
                        pct = 100.0 * done / total
                        print(f"    … {done // (1024 * 1024)} MiB ({pct:.0f}%)", flush=True)
    except urllib.error.HTTPError as e:
        raise SystemExit(f"download {region.id} failed: HTTP {e.code} {url}") from e
    except urllib.error.URLError as e:
        raise SystemExit(f"download {region.id} failed: {e.reason}") from e

    size = tmp.stat().st_size
    if size < 1024 * 1024:
        head = tmp.read_bytes()[:32]
        tmp.unlink(missing_ok=True)
        if head.startswith(b"<!DOCTYPE") or head.startswith(b"<html"):
            raise SystemExit(f"download {region.id}: получен HTML вместо PBF — проверьте URL {url}")
        raise SystemExit(f"download {region.id}: файл слишком мал ({size} bytes)")

    tmp.replace(dest)
    print(f"  saved {dest} ({dest.stat().st_size // (1024 * 1024)} MiB)", flush=True)
    return dest


def download_regions(
    manifest: GeofabrikManifest,
    region_ids: set[str] | None = None,
    *,
    include_optional: bool = False,
    force: bool = False,
) -> list[Path]:
    paths: list[Path] = []
    for region in manifest.regions:
        if region_ids is not None and region.id not in region_ids:
            continue
        if region.optional and not include_optional:
            print(f"  skip optional {region.id} (use --include-optional)", flush=True)
            continue
        paths.append(download_pbf(manifest, region, force=force))
    return paths

#!/usr/bin/env python3
"""Случайная выборка из osm_esr_index.parquet для визуальной проверки."""

from __future__ import annotations

import argparse
import random
import sys
from collections import defaultdict
from pathlib import Path

from stations_etl.paths import OSM_INDEX_PARQUET

DEFAULT_INPUT = OSM_INDEX_PARQUET

DISPLAY_COLS = (
    "esr6",
    "name_osm",
    "lat",
    "lon",
    "pbf_region",
    "match_method",
    "confidence",
)


def load_rows(path: Path) -> list[dict]:
    try:
        import pyarrow.parquet as pq
    except ImportError as e:
        raise SystemExit("sample_osm: нужен pyarrow") from e
    if not path.is_file():
        raise SystemExit(f"sample_osm: файл не найден: {path}")
    return pq.read_table(path).to_pylist()


def stratified_sample(rows: list[dict], n: int, seed: int, key: str) -> list[dict]:
    if n >= len(rows):
        return list(rows)
    rng = random.Random(seed)
    by: dict[str, list[dict]] = defaultdict(list)
    for row in rows:
        by[str(row.get(key, "unknown"))].append(row)
    picked: list[dict] = []
    seen: set[int] = set()
    for g in sorted(by.keys()):
        if len(picked) >= n:
            break
        row = rng.choice(by[g])
        seen.add(id(row))
        picked.append(row)
    pool = [r for r in rows if id(r) not in seen]
    rng.shuffle(pool)
    for row in pool:
        if len(picked) >= n:
            break
        picked.append(row)
    return picked


def format_table(sample: list[dict]) -> str:
    if not sample:
        return "(пусто)"
    widths = {c: len(c) for c in DISPLAY_COLS}
    srows = []
    for row in sample:
        sr = {c: str(row.get(c, ""))[:48] for c in DISPLAY_COLS}
        srows.append(sr)
        for c in DISPLAY_COLS:
            widths[c] = max(widths[c], len(sr[c]))
    lines = [
        " | ".join(c.ljust(widths[c]) for c in DISPLAY_COLS),
        "-+-".join("-" * widths[c] for c in DISPLAY_COLS),
    ]
    lines.extend(" | ".join(sr[c].ljust(widths[c]) for c in DISPLAY_COLS) for sr in srows)
    return "\n".join(lines)


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--input", type=Path, default=DEFAULT_INPUT)
    p.add_argument("--n", type=int, default=20)
    p.add_argument("--seed", type=int, default=42)
    args = p.parse_args()
    rows = load_rows(args.input)
    sample = stratified_sample(rows, args.n, args.seed, "pbf_region")
    print(f"# {args.input} ({len(rows)} esr6)")
    print(f"# sample n={len(sample)} seed={args.seed}")
    print()
    print(format_table(sample))
    return 0


if __name__ == "__main__":
    sys.exit(main())

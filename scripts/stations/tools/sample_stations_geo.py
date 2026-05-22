#!/usr/bin/env python3
"""Случайная выборка из stations_geo.sqlite для визуальной проверки."""

from __future__ import annotations

import argparse
import random
import sqlite3
import sys
from collections import defaultdict
from pathlib import Path

from stations_etl.paths import GEO_SQLITE

DEFAULT_DB = GEO_SQLITE

DISPLAY = ("esr6", "name", "lat", "lon", "region_group", "country_hint", "match_method", "confidence")


def load_rows(path: Path) -> list[dict]:
    if not path.is_file():
        raise SystemExit(f"sample_geo: не найден {path}")
    conn = sqlite3.connect(path)
    conn.row_factory = sqlite3.Row
    try:
        cur = conn.execute(
            f"SELECT {', '.join(DISPLAY)} FROM stations_geo ORDER BY esr6"
        )
        return [dict(r) for r in cur.fetchall()]
    finally:
        conn.close()


def stratified(rows: list[dict], n: int, seed: int) -> list[dict]:
    if n >= len(rows):
        return rows
    rng = random.Random(seed)
    by: dict[str, list[dict]] = defaultdict(list)
    for r in rows:
        by[str(r.get("region_group", "unknown"))].append(r)
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
    widths = {c: len(c) for c in DISPLAY}
    srows = []
    for row in sample:
        sr = {c: str(row.get(c, ""))[:40] for c in DISPLAY}
        srows.append(sr)
        for c in DISPLAY:
            widths[c] = max(widths[c], len(sr[c]))
    lines = [
        " | ".join(c.ljust(widths[c]) for c in DISPLAY),
        "-+-".join("-" * widths[c] for c in DISPLAY),
    ]
    lines.extend(" | ".join(sr[c].ljust(widths[c]) for c in DISPLAY) for sr in srows)
    return "\n".join(lines)


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--input", type=Path, default=DEFAULT_DB)
    p.add_argument("--n", type=int, default=20)
    p.add_argument("--seed", type=int, default=42)
    args = p.parse_args()
    rows = load_rows(args.input)
    sample = stratified(rows, args.n, args.seed)
    print(f"# {args.input} ({len(rows)} stations)")
    print(f"# sample n={len(sample)} seed={args.seed}")
    print()
    print(format_table(sample))
    return 0


if __name__ == "__main__":
    sys.exit(main())

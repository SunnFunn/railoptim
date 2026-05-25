#!/usr/bin/env python3
"""
Случайная выборка из stations_nsi_raw.parquet для визуальной проверки.

  python3 sample_nsi_parquet.py
  python3 sample_nsi_parquet.py --n 30 --seed 42
  python3 sample_nsi_parquet.py --input /path/to/stations_nsi_raw.parquet --check
"""

from __future__ import annotations

import argparse
import random
import sys
from collections import defaultdict
from pathlib import Path

from stations_etl.paths import NSI_PARQUET

DEFAULT_INPUT = NSI_PARQUET

DISPLAY_COLS = (
    "esr6",
    "name_nsi",
    "railway_rw",
    "country_hint",
    "region_group",
    "network_district",
    "checksum_valid",
)

KNOWN_REGION_GROUPS = frozenset(
    {"ru", "cis", "baltic", "china_mongolia", "south_caucasus", "unknown"}
)


def load_rows(path: Path) -> list[dict]:
    try:
        import pyarrow.parquet as pq
    except ImportError as e:
        raise SystemExit(
            "sample_nsi: нужен pyarrow (scripts/stations/requirements-stations.txt)"
        ) from e

    if not path.is_file():
        raise SystemExit(f"sample_nsi: файл не найден: {path}")

    table = pq.read_table(path)
    return table.to_pylist()


def stratified_sample(rows: list[dict], n: int, seed: int) -> list[dict]:
    if n <= 0 or not rows:
        return []
    if n >= len(rows):
        return list(rows)

    rng = random.Random(seed)
    by_group: dict[str, list[dict]] = defaultdict(list)
    for row in rows:
        by_group[str(row.get("region_group", "unknown"))].append(row)

    picked: list[dict] = []
    picked_ids: set[int] = set()

    def take(row: dict) -> None:
        rid = id(row)
        if rid not in picked_ids:
            picked_ids.add(rid)
            picked.append(row)

    groups = sorted(by_group.keys())
    for g in groups:
        if len(picked) >= n:
            break
        take(rng.choice(by_group[g]))

    pool = [r for r in rows if id(r) not in picked_ids]
    rng.shuffle(pool)
    for row in pool:
        if len(picked) >= n:
            break
        take(row)

    return picked


def random_sample(rows: list[dict], n: int, seed: int) -> list[dict]:
    if n >= len(rows):
        return list(rows)
    rng = random.Random(seed)
    return rng.sample(rows, n)


def validate_sample(rows: list[dict], sample: list[dict]) -> None:
    errors: list[str] = []
    for row in sample:
        esr6 = str(row.get("esr6", ""))
        if len(esr6) != 6 or not esr6.isdigit():
            errors.append(f"invalid esr6: {esr6!r}")
        name = str(row.get("name_nsi", "")).strip()
        if not name:
            errors.append(f"empty name_nsi for esr6={esr6}")
        rg = str(row.get("region_group", ""))
        if rg not in KNOWN_REGION_GROUPS:
            errors.append(f"unknown region_group {rg!r} for esr6={esr6}")
        for col in DISPLAY_COLS:
            if col not in row:
                errors.append(f"missing column {col} for esr6={esr6}")

    if not sample and rows:
        errors.append("sample is empty but parquet has rows")

    if errors:
        raise SystemExit("sample_nsi --check failed:\n  " + "\n  ".join(errors))


def format_table(sample: list[dict]) -> str:
    if not sample:
        return "(пустая выборка)"

    cols = DISPLAY_COLS
    widths = {c: len(c) for c in cols}
    str_rows: list[dict[str, str]] = []
    for row in sample:
        srow = {c: str(row.get(c, "")) for c in cols}
        str_rows.append(srow)
        for c in cols:
            widths[c] = max(widths[c], len(srow[c]))

    def fmt_row(values: dict[str, str]) -> str:
        return " | ".join(values[c].ljust(widths[c]) for c in cols)

    lines = [fmt_row({c: c for c in cols}), "-+-".join("-" * widths[c] for c in cols)]
    lines.extend(fmt_row(r) for r in str_rows)
    return "\n".join(lines)


def summarize(rows: list[dict]) -> str:
    by_rg: dict[str, int] = defaultdict(int)
    for row in rows:
        by_rg[str(row.get("region_group", "unknown"))] += 1
    parts = [f"{k}={v}" for k, v in sorted(by_rg.items())]
    return f"всего строк: {len(rows)}; region_group: {', '.join(parts)}"


def main() -> int:
    parser = argparse.ArgumentParser(description="Случайная выборка из NSI parquet")
    parser.add_argument("--input", type=Path, default=DEFAULT_INPUT, help="parquet файл")
    parser.add_argument("--n", type=int, default=20, help="размер выборки")
    parser.add_argument("--seed", type=int, default=42, help="seed для воспроизводимости")
    parser.add_argument(
        "--no-stratified",
        action="store_true",
        help="чистый random.sample без минимума по region_group",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="sanity-check выборки (для автотестов); без печати таблицы",
    )
    args = parser.parse_args()

    rows = load_rows(args.input)
    if args.no_stratified:
        sample = random_sample(rows, args.n, args.seed)
    else:
        sample = stratified_sample(rows, args.n, args.seed)

    if args.check:
        validate_sample(rows, sample)
        print(f"sample check OK ({len(sample)} rows from {args.input})")
        return 0

    print(f"# {args.input}")
    print(f"# {summarize(rows)}")
    print(f"# выборка n={len(sample)} seed={args.seed} stratified={not args.no_stratified}")
    print()
    print(format_table(sample))
    return 0


if __name__ == "__main__":
    sys.exit(main())

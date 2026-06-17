#!/usr/bin/env python3
"""
Заполнение StationFromCode / StationToCode в JSON-выгрузке register.

Читает массив строк реестра из stdin, подключается к MSSQL (переменные как mssql.py),
строит индекс NSI.Station по (имя станции, краткое имя дороги) → ЕСР-6, пишет JSON в stdout.

Статистика — одна строка на stderr: OKPO_ESR_STATS=<json>
"""

from __future__ import annotations

import json
import sys
from collections import Counter
from typing import Any

from mssql import fetch_nsi_station_rows
from normalize import (
    normalize_esr6,
    normalize_name,
    normalize_railway_rw,
    station_lookup_keys,
)

StationKey = tuple[str, str | None]  # (name_lookup_key, railway_rw)


def build_station_index(
    nsi_rows: list[tuple[Any, Any, Any]],
) -> tuple[dict[StationKey, str], dict[str, Any]]:
    """(name, railway_rw) -> esr6; отчёт о конфликтах."""
    index: dict[StationKey, str] = {}
    conflicts: list[dict[str, str]] = []
    rejected = 0

    for row in nsi_rows:
        if len(row) < 2:
            rejected += 1
            continue
        code6_raw, name_raw = row[0], row[1]
        rw_raw = row[2] if len(row) > 2 else None
        esr6 = normalize_esr6(code6_raw)
        if len(esr6) != 6 or not esr6.isdigit():
            rejected += 1
            continue
        display_name = normalize_name(name_raw)
        if not display_name:
            rejected += 1
            continue
        rw = normalize_railway_rw(rw_raw)
        keys = station_lookup_keys(name_raw)
        if not keys:
            rejected += 1
            continue
        for name_key in keys:
            key: StationKey = (name_key, rw)
            prev = index.get(key)
            if prev is not None and prev != esr6:
                conflicts.append(
                    {
                        "name": display_name,
                        "name_key": name_key,
                        "railway_rw": rw or "",
                        "existing": prev,
                        "new": esr6,
                    }
                )
                continue
            index[key] = esr6

    report: dict[str, Any] = {
        "nsi_rows": len(nsi_rows),
        "index_size": len(index),
        "rejected": rejected,
        "conflicts": len(conflicts),
        "conflicts_sample": conflicts[:50],
    }
    return index, report


def lookup_esr(
    index: dict[StationKey, str],
    station_name: str,
    railroad_name: str,
) -> str | None:
    rw = normalize_railway_rw(railroad_name)
    for name_key in station_lookup_keys(station_name):
        code = index.get((name_key, rw))
        if code is not None:
            return code
    # Без fallback «единственное имя на всех дорогах» — иначе чужой ЕСР блокировал веб-агент.
    return None


def fill_rows(
    rows: list[dict[str, Any]],
    index: dict[StationKey, str],
) -> dict[str, Any]:
    matched_from = 0
    matched_to = 0
    missing_from = 0
    missing_to = 0
    unique_queries: Counter[tuple[str, str, str]] = Counter()

    for row in rows:
        s_from = row.get("StationFromName") or ""
        rf = row.get("RailroadFromName") or ""
        s_to = row.get("StationToName") or ""
        rt = row.get("RailroadToName") or ""

        code_from = lookup_esr(index, s_from, rf)
        code_to = lookup_esr(index, s_to, rt)

        row["StationFromCode"] = code_from
        row["StationToCode"] = code_to

        if code_from:
            matched_from += 1
        elif normalize_name(s_from):
            missing_from += 1
            unique_queries[("from", normalize_name(s_from), normalize_railway_rw(rf) or "")] += 1

        if code_to:
            matched_to += 1
        elif normalize_name(s_to):
            missing_to += 1
            unique_queries[("to", normalize_name(s_to), normalize_railway_rw(rt) or "")] += 1

    missing_unique = len(unique_queries)
    missing_sample = [
        {"side": side, "station": st, "railway": rw, "count": cnt}
        for (side, st, rw), cnt in unique_queries.most_common(30)
    ]

    return {
        "rows": len(rows),
        "matched_from": matched_from,
        "matched_to": matched_to,
        "missing_from": missing_from,
        "missing_to": missing_to,
        "missing_unique_pairs": missing_unique,
        "missing_sample": missing_sample,
    }


def main() -> int:
    try:
        rows = json.load(sys.stdin)
    except json.JSONDecodeError as e:
        print(f"esr_fill: невалидный JSON на stdin: {e}", file=sys.stderr)
        return 2

    if not isinstance(rows, list):
        print("esr_fill: ожидается JSON-массив строк реестра", file=sys.stderr)
        return 2

    nsi_rows = fetch_nsi_station_rows()
    index, index_report = build_station_index(nsi_rows)
    fill_report = fill_rows(rows, index)
    stats = {"index": index_report, "fill": fill_report}
    print(f"OKPO_ESR_STATS={json.dumps(stats, ensure_ascii=False)}", file=sys.stderr)

    json.dump(rows, sys.stdout, ensure_ascii=False, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Подбор кодов ЕСР-6 для станций погрузки по имени станции + дороге через MSSQL.

Самодостаточный хелпер для бинарника `railoptim-load-stations`:
  - читает с stdin JSON-массив `[{"station": ..., "railway": ...}, ...]`;
  - строит индекс NSI.Station (имя + краткое имя дороги) -> ЕСР-6;
  - пишет в stdout JSON-массив того же порядка `[{"station", "railway", "code"}]`
    (code — строка из 6 цифр либо null);
  - статистика одной строкой на stderr: LOAD_STATIONS_ESR_STATS=<json>.

Подключение к MSSQL — те же переменные окружения, что и в других скриптах проекта
(src/data/dislocations.py, src/data/wash.py): MSSQL_SERVER_MSKASUVPL, DOMAIN_USER,
PASSWORD, MSSQL_DB_ASUVP, MSSQL_DOMAIN (секреты Infisical).
"""

from __future__ import annotations

import json
import os
import re
import sys
from collections import Counter
from typing import Any

try:
    import pymssql
except ImportError:  # pragma: no cover
    pymssql = None  # type: ignore[assignment]


# --------------------------------------------------------------------------- #
# Подключение к MSSQL
# --------------------------------------------------------------------------- #
def _env(key: str, default: str | None = None) -> str | None:
    v = os.environ.get(key)
    if v is None or v == "":
        return default
    return v


def _mssql_connect() -> Any:
    if pymssql is None:
        print(
            "load_stations_esr: нужен pymssql (как для src/data/dislocations.py)",
            file=sys.stderr,
        )
        sys.exit(1)

    server = _env("MSSQL_SERVER_MSKASUVPL")
    if not server:
        print(
            "load_stations_esr: задайте MSSQL_SERVER_MSKASUVPL (секрет Infisical)",
            file=sys.stderr,
        )
        sys.exit(1)

    user = _env("DOMAIN_USER", "") or ""
    password = _env("PASSWORD", "") or ""
    database = _env("MSSQL_DB_ASUVP", "") or ""
    domain = _env("MSSQL_DOMAIN", "") or ""

    return pymssql.connect(
        server=server,
        user=domain + user,
        password=password,
        database=database,
    )


NSI_STATION_SQL = """
SELECT S.Code6, S.Name, R.ShortName
FROM NSI.Station S (NOLOCK)
JOIN NSI.RailWay R (NOLOCK) ON S.RailWayId = R.RailWayId
"""


def fetch_nsi_station_rows() -> list[tuple[Any, Any, Any]]:
    conn = _mssql_connect()
    cur = conn.cursor()
    try:
        cur.execute(NSI_STATION_SQL)
        return list(cur.fetchall())
    finally:
        cur.close()
        conn.close()


# --------------------------------------------------------------------------- #
# Нормализация имён станций / дорог / кодов ЕСР
# --------------------------------------------------------------------------- #
_WHITESPACE = re.compile(r"\s+")
_QUOTES = re.compile(r"[«»\"'`]+")

# Суффиксы через дефис: сокращения в Excel -> полное имя в NSI.
_ABBREV_SUFFIX: tuple[tuple[re.Pattern[str], str], ...] = (
    (re.compile(r"(?i)-тов\.?$"), "-Товарный"),
    (re.compile(r"(?i)-гл\.?$"), "-Главный"),
    (re.compile(r"(?i)-сорт\.?$"), "-Сортировочная"),
    (re.compile(r"(?i)-пасс\.?$"), "-Пассажирский"),
)

# Хвост: римская цифра как отдельное слово (Краснодар I -> Краснодар 1).
_ROMAN_END = (
    (re.compile(r"\s+III\s*$", re.I), " 3"),
    (re.compile(r"\s+II\s*$", re.I), " 2"),
    (re.compile(r"\s+I\s*$", re.I), " 1"),
)


def normalize_name(raw: Any) -> str:
    """Trim и схлопывание пробелов (как в ячейке / NSI для отображения)."""
    if raw is None:
        return ""
    s = str(raw).replace("\u00a0", " ").strip()
    return _WHITESPACE.sub(" ", s)


def canonicalize_station_name(raw: Any) -> str:
    """Приведение к каноническому виду перед casefold (Excel <-> NSI)."""
    s = normalize_name(raw)
    if not s:
        return ""

    s = _QUOTES.sub(" ", s)
    s = _WHITESPACE.sub(" ", s).strip()

    # Код ЕСР в скобках в конце наименования.
    s = re.sub(r"\s*\(\d{6}\)\s*$", "", s)

    for pat, repl in _ABBREV_SUFFIX:
        s = pat.sub(repl, s)

    s = re.sub(r"(?i)\bсортировочная\b", "сортировка", s)

    for pat, repl in _ROMAN_END:
        s = pat.sub(repl, s)

    return _WHITESPACE.sub(" ", s).strip()


def station_lookup_keys(raw: Any) -> list[str]:
    """Ключи поиска (casefold): канонический и исходный."""
    display = normalize_name(raw)
    if not display:
        return []

    keys: list[str] = []

    def add(text: str) -> None:
        k = text.casefold()
        if k and k not in keys:
            keys.append(k)

    add(canonicalize_station_name(display))
    add(display)
    return keys


def normalize_railway_rw(raw: Any) -> str | None:
    if raw is None:
        return None
    s = str(raw).strip().upper()
    return s or None


def normalize_esr6(code6_raw: Any) -> str:
    if code6_raw is None or isinstance(code6_raw, bool):
        return ""
    if isinstance(code6_raw, int):
        digits = str(abs(code6_raw))
    elif isinstance(code6_raw, float):
        digits = str(int(code6_raw))
    else:
        digits = "".join(c for c in str(code6_raw).strip() if c.isdigit())
    if not digits:
        return ""
    if len(digits) > 6:
        digits = digits[-6:]
    return digits.zfill(6)


# --------------------------------------------------------------------------- #
# Индекс и поиск
# --------------------------------------------------------------------------- #
StationKey = tuple[str, str | None]  # (name_lookup_key, railway_rw)


def build_station_index(
    nsi_rows: list[tuple[Any, Any, Any]],
) -> tuple[dict[StationKey, str], dict[str, Any]]:
    """(name_key, railway_rw) -> esr6; конфликты разных кодов отбрасываются."""
    index: dict[StationKey, str] = {}
    rejected = 0
    conflicts = 0

    for row in nsi_rows:
        if len(row) < 2:
            rejected += 1
            continue
        esr6 = normalize_esr6(row[0])
        if len(esr6) != 6 or not esr6.isdigit():
            rejected += 1
            continue
        rw = normalize_railway_rw(row[2] if len(row) > 2 else None)
        keys = station_lookup_keys(row[1])
        if not keys:
            rejected += 1
            continue
        for name_key in keys:
            key: StationKey = (name_key, rw)
            prev = index.get(key)
            if prev is not None and prev != esr6:
                conflicts += 1
                continue
            index[key] = esr6

    report = {
        "nsi_rows": len(nsi_rows),
        "index_size": len(index),
        "rejected": rejected,
        "conflicts": conflicts,
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
    return None


def main() -> int:
    try:
        rows = json.load(sys.stdin)
    except json.JSONDecodeError as e:
        print(f"load_stations_esr: невалидный JSON на stdin: {e}", file=sys.stderr)
        return 2
    if not isinstance(rows, list):
        print("load_stations_esr: ожидается JSON-массив", file=sys.stderr)
        return 2

    nsi_rows = fetch_nsi_station_rows()
    index, index_report = build_station_index(nsi_rows)

    matched = 0
    missing: Counter[tuple[str, str]] = Counter()
    out: list[dict[str, Any]] = []
    for row in rows:
        station = (row or {}).get("station") or ""
        railway = (row or {}).get("railway") or ""
        code = lookup_esr(index, station, railway)
        if code:
            matched += 1
        else:
            missing[(normalize_name(station), normalize_railway_rw(railway) or "")] += 1
        out.append({"station": station, "railway": railway, "code": code})

    stats = {
        "index": index_report,
        "queries": len(rows),
        "matched": matched,
        "missing": len(rows) - matched,
        "missing_sample": [
            {"station": st, "railway": rw, "count": cnt}
            for (st, rw), cnt in missing.most_common(30)
        ],
    }
    print(f"LOAD_STATIONS_ESR_STATS={json.dumps(stats, ensure_ascii=False)}", file=sys.stderr)

    json.dump(out, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())

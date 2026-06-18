#!/usr/bin/env python3
"""Данные MSSQL для справочника свободных ёмкостей подъездных путей станций погрузки.

Запускается из основного бинарника `railoptim` (см. src/data/free_loadroads.rs)
в суточном прогоне; секреты MSSQL_* берутся из окружения (Infisical через run.sh).

Stdout — JSON-объект:
  {
    "load_station_codes": ["612408", ...],   # Шаг 1: станции погрузки зерна за 6 мес.
    "cars_on_station":    {"612408": 12, ...} # Шаг 2: вагоны на станции (Distance=0)
  }
Stderr — одна строка статистики: FREE_LOADROADS_STATS=<json>.

Шаг 1 — БД MSSQL_DB_SLP (станции погрузки, ЕТСНГ зерновых, парк != Инвентарный,
статус «Согласованный», LoadDate за последние 6 месяцев).
Шаг 2 — БД MSSQL_DB_ASUVP (DislocationPreview, Distance=0: вагоны уже на станции).
"""

from __future__ import annotations

import calendar
import json
import sys
from datetime import date
from typing import Any

try:
    import pymssql
except ImportError:  # pragma: no cover
    pymssql = None  # type: ignore[assignment]

import os


def _env(key: str, default: str | None = None) -> str | None:
    v = os.environ.get(key)
    if v is None or v == "":
        return default
    return v


def _connect(database_env: str) -> Any:
    """Подключение к MSSQL; имя БД — из переменной окружения `database_env`."""
    if pymssql is None:
        print("free_loadroads: нужен pymssql (как для dislocations.py)", file=sys.stderr)
        sys.exit(1)

    server = _env("MSSQL_SERVER_MSKASUVPL")
    if not server:
        print("free_loadroads: задайте MSSQL_SERVER_MSKASUVPL (секрет Infisical)", file=sys.stderr)
        sys.exit(1)

    database = _env(database_env, "") or ""
    if not database:
        print(f"free_loadroads: не задана БД {database_env}", file=sys.stderr)
        sys.exit(1)

    user = _env("DOMAIN_USER", "") or ""
    password = _env("PASSWORD", "") or ""
    domain = _env("MSSQL_DOMAIN", "") or ""
    return pymssql.connect(
        server=server,
        user=domain + user,
        password=password,
        database=database,
    )


def _months_ago(d: date, n: int) -> date:
    """Дата на n месяцев назад (с зажимом дня по длине месяца)."""
    month_index = d.month - 1 - n
    year = d.year + month_index // 12
    month = month_index % 12 + 1
    day = min(d.day, calendar.monthrange(year, month)[1])
    return date(year, month, day)


def _normalize_esr6(raw: Any) -> str:
    if raw is None:
        return ""
    if isinstance(raw, bool):
        return ""
    if isinstance(raw, int):
        digits = str(abs(raw))
    elif isinstance(raw, float):
        digits = str(int(raw))
    else:
        digits = "".join(c for c in str(raw).strip() if c.isdigit())
    if not digits:
        return ""
    if len(digits) > 6:
        digits = digits[-6:]
    return digits.zfill(6)


# Шаг 1: станции погрузки за период (даты подставляются f-строкой, не пользовательский ввод).
def _load_stations_sql(date_from: str, date_to: str) -> str:
    return f"""
    SELECT
        ACL.RailwayFromShortName, ACL.StationFromName, SF.Code6,
        ACL.ETSNGName, ACL.ETSNGCode,
        SUM(CLD.CarCount) AS TotalCars
    FROM dbo.vwASUVPClaim ACL (NOLOCK)
    JOIN dbo.Claim CL (NOLOCK) ON CL.ClaimId = ACL.Id
    JOIN dbo.ClaimLoadingSchedule CLS (NOLOCK) ON CLS.ClaimId = ACL.Id
    JOIN dbo.ClaimLoadingScheduleDate CLD (NOLOCK) ON CLS.Id = CLD.LoadingScheduleId
    JOIN NSI.Station SF (NOLOCK)  ON SF.Id = CL.StationFromId
    JOIN NSI.Station ST (NOLOCK)  ON ST.Id = CL.StationToId
    JOIN dbo.Company CPF (NOLOCK) ON CPF.Id = CL.LoaderFromId
    WHERE
        CLS.Version = 1
        AND ACL.ScheduleStatusName = 'Согласованный'
        AND CLD.LoadDate <= '{date_to}'
        AND CLD.LoadDate >= '{date_from}'
        AND ACL.CarParkName != 'Инвентарный'
        AND (ACL.ETSNGCode LIKE '0%' OR ACL.ETSNGCode LIKE '5%')
    GROUP BY
        ACL.RailwayFromShortName, ACL.StationFromName, SF.Code6, ACL.ETSNGName, ACL.ETSNGCode;
    """


# Шаг 2: вагоны, уже находящиеся на станции (Distance=0).
CARS_ON_STATION_SQL = """
SELECT
    DP.StationToCode,
    COUNT(DP.CarNumber) AS Cars
FROM DislocationPreview DP (NOLOCK)
WHERE DP.Distance = 0
GROUP BY
    DP.StationToCode;
"""


def fetch_load_station_codes(date_from: str, date_to: str) -> set[str]:
    conn = _connect("MSSQL_DB_SLP")
    cur = conn.cursor()
    try:
        cur.execute(_load_stations_sql(date_from, date_to))
        codes: set[str] = set()
        for row in cur.fetchall():
            code = _normalize_esr6(row[2] if len(row) > 2 else None)
            if len(code) == 6:
                codes.add(code)
        return codes
    finally:
        cur.close()
        conn.close()


def fetch_cars_on_station() -> dict[str, int]:
    conn = _connect("MSSQL_DB_ASUVP")
    cur = conn.cursor()
    try:
        cur.execute(CARS_ON_STATION_SQL)
        cars: dict[str, int] = {}
        for row in cur.fetchall():
            code = _normalize_esr6(row[0] if row else None)
            if len(code) != 6:
                continue
            try:
                n = int(row[1]) if row[1] is not None else 0
            except (TypeError, ValueError):
                n = 0
            # Несколько исходных кодов могут нормализоваться в один — суммируем.
            cars[code] = cars.get(code, 0) + n
        return cars
    finally:
        cur.close()
        conn.close()


def main() -> int:
    today = date.today()
    date_to = today.isoformat()
    date_from = _months_ago(today, 6).isoformat()

    codes = fetch_load_station_codes(date_from, date_to)
    cars = fetch_cars_on_station()

    stats = {
        "date_from": date_from,
        "date_to": date_to,
        "load_station_codes": len(codes),
        "cars_on_station_rows": len(cars),
        "cars_total": sum(cars.values()),
    }
    print(f"FREE_LOADROADS_STATS={json.dumps(stats, ensure_ascii=False)}", file=sys.stderr)

    json.dump(
        {"load_station_codes": sorted(codes), "cars_on_station": cars},
        sys.stdout,
        ensure_ascii=False,
    )
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())

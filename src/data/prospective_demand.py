#!/usr/bin/env python3
"""Перспективный спрос на распыление: предварительные заявки SLP на 1–20 следующего месяца.

Запускается из railoptim (src/data/spray.rs). Секреты — как у gu12.py:
  MSSQL_SERVER_MSKASUVPL, MSSQL_DB_SLP, DOMAIN_USER, PASSWORD, MSSQL_DOMAIN (опц.)

Аргументы: --from YYYY-MM-DD --to YYYY-MM-DD (включительно).
Stdout — JSON-массив строк заявки (станция погрузки, ЕТСНГ, вагоны).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from typing import Any

try:
    import pymssql
except ImportError:  # pragma: no cover
    pymssql = None  # type: ignore[assignment]


def _env(key: str, default: str | None = None) -> str | None:
    v = os.environ.get(key)
    if v is None or v == "":
        return default
    return v


def _connect() -> Any:
    if pymssql is None:
        print("prospective_demand: нужен pymssql (как для gu12.py)", file=sys.stderr)
        sys.exit(1)
    server = _env("MSSQL_SERVER_MSKASUVPL")
    if not server:
        print("prospective_demand: задайте MSSQL_SERVER_MSKASUVPL", file=sys.stderr)
        sys.exit(1)
    database = _env("MSSQL_DB_SLP", "") or ""
    if not database:
        print("prospective_demand: не задана БД MSSQL_DB_SLP", file=sys.stderr)
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


def _normalize_esr6(raw: Any) -> str:
    if raw is None or isinstance(raw, bool):
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


_DATE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


def build_sql(date_from: str, date_to: str) -> str:
    if not _DATE.match(date_from) or not _DATE.match(date_to):
        raise ValueError("даты только YYYY-MM-DD")
    return f"""
    SELECT
        ACL.RailwayFromShortName, ACL.StationFromName, SF.Code6,
        ACL.ETSNGName, ACL.ETSNGCode, ACL.ScheduleStatusName,
        SUM(CLD.CarCount) AS TotalCars
    FROM dbo.vwASUVPClaim ACL (NOLOCK)
        JOIN dbo.ClaimLoadingSchedule CLS (NOLOCK) ON CLS.ClaimId = ACL.Id
        JOIN dbo.ClaimLoadingScheduleDate CLD (NOLOCK) ON CLS.Id = CLD.LoadingScheduleId
        JOIN NSI.Station SF (NOLOCK) ON SF.Id = ACL.StationFromId
    WHERE
        CLS.Version = 1
        AND ACL.ScheduleStatusName = N'Предварительный'
        AND CLD.LoadDate <= '{date_to}'
        AND CLD.LoadDate >= '{date_from}'
        AND (ACL.ETSNGCode LIKE '0%' OR ACL.ETSNGCode LIKE '5%')
        AND ACL.CarParkName != N'Инвентарный'
    GROUP BY
        ACL.RailwayFromShortName, ACL.StationFromName, SF.Code6,
        ACL.ScheduleStatusName, ACL.ETSNGName, ACL.ETSNGCode;
    """


def fetch(date_from: str, date_to: str) -> list[dict[str, Any]]:
    conn = _connect()
    cur = conn.cursor()
    try:
        cur.execute(build_sql(date_from, date_to))
        rows: list[dict[str, Any]] = []
        for row in cur.fetchall():
            code = _normalize_esr6(row[2] if len(row) > 2 else None)
            if len(code) != 6:
                continue
            try:
                cars = int(row[6] or 0)
            except (TypeError, ValueError):
                cars = 0
            if cars <= 0:
                continue
            rows.append(
                {
                    "railway": (row[0] or "").strip(),
                    "station_name": (row[1] or "").strip(),
                    "station_code": code,
                    "etsng_name": (row[3] or "").strip(),
                    "etsng_code": (row[4] or "").strip(),
                    "cars": cars,
                }
            )
        return rows
    finally:
        cur.close()
        conn.close()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--from", dest="date_from", required=True)
    parser.add_argument("--to", dest="date_to", required=True)
    args = parser.parse_args()
    json.dump(fetch(args.date_from, args.date_to), sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""
Станции промывки (SLP, MSSQL) → JSON в stdout для Rust.

  python3 wash.py json

Переменные окружения:
  MSSQL_SERVER или MSSQL_HOST
  MSSQL_USER
  MSSQL_PASSWORD
  MSSQL_DATABASE
  MSSQL_DOMAIN   (опционально, префикс к логину)
"""

from __future__ import annotations

import json
import os
import sys


def _env(key: str, default: str | None = None) -> str | None:
    v = os.environ.get(key)
    if v is None or v == "":
        return default
    return v


# Тело SQL-запроса не менять (согласовано с SLP / AgreementWashingStationView).
_WASH_SQL = '''
    SELECT RANKED.Firm, RANKED.RailWayWashDivision, RANKED.RailWayWash, RANKED.RailWayWashCode,
        RANKED.StationWash, RANKED.StationWashCode, RANKED.WashCapacity
    FROM
        (
        SELECT
            F.ShortName AS Firm, RP.Name AS RailWayWashDivision, R.ShortName AS RailWayWash, R.Code AS RailWayWashCode,
            S.Name AS StationWash, S.Code6 AS StationWashCode, AWS.CarCountPerDay AS WashCapacity,
            ROW_NUMBER() OVER (PARTITION BY AWS.calc_TargetFirmId, AWS.calc_StationCode6 \
                ORDER BY AWS.CreatedDateTime DESC) AS Rank
        FROM AgreementWashingStationView AWS (NOLOCK)
        JOIN NSI.Station (NOLOCK) S ON S.StationId = AWS.StationId
        JOIN Firm (NOLOCK) F ON F.FirmId = AWS.calc_TargetFirmId
        JOIN NSI.RailWay (NOLOCK) R ON R.RailWayId = S.RailWayId
        LEFT JOIN NSI.RailWayPart (NOLOCK) RP ON RP.RailWayPartId = S.RailWayPartId
        WHERE (AWS.calc_DateEnd IS NULL OR DATALENGTH(AWS.calc_DateEnd) = 0 OR LEN(AWS.calc_DateEnd) = 0)
        AND AWS.IsDeleted = 0
        ) RANKED
    WHERE RANKED.Rank = 1;
    '''


def _conndict_from_env() -> dict:
    server = _env("MSSQL_SERVER_MSKASUVPL")
    user = _env("DOMAIN_USER")
    password = _env("PASSWORD")
    database = _env("MSSQL_DB_ASUVP")
    domain = _env("MSSQL_DOMAIN", "") or ""
    if not all([server, user, password, database]):
        sys.stderr.write(
            "Задайте MSSQL_SERVER|MSSQL_HOST, MSSQL_USER, MSSQL_PASSWORD, MSSQL_DATABASE\n"
        )
        raise SystemExit(2)
    return {
        "server": server,
        "domain": domain,
        "user": user,
        "password": password,
        "database": database,
    }


def query_wash_stations() -> list[dict]:
    """Читает станции промывки из MSSQL (переменные окружения)."""
    import pymssql

    c = _conndict_from_env()
    conn = pymssql.connect(
        server=c["server"],
        user=(c["domain"] + c["user"]),
        password=c["password"],
        database=c["database"],
    )
    try:
        cur = conn.cursor()
        try:
            cur.execute(_WASH_SQL)
            rows = cur.fetchall()
        finally:
            cur.close()
    finally:
        conn.close()

    out: list[dict] = []
    for row in rows:
        cap = row[6]
        try:
            cap_int = int(cap) if cap is not None else 0
        except (TypeError, ValueError):
            cap_int = 0
        out.append(
            {
                "RailWayWashDivision": row[1],
                "RailWayWash": row[2],
                "RailWayWashCode": "" if row[3] is None else str(row[3]).strip(),
                "StationWash": row[4],
                "StationWashCode": "" if row[5] is None else str(row[5]).strip(),
                "WashCapacity": cap_int,
            }
        )
    out.sort(key=lambda x: (str(x.get("RailWayWash") or ""), str(x.get("StationWashCode") or "")))
    return out


def main() -> None:
    if len(sys.argv) < 2 or sys.argv[1] != "json":
        sys.stderr.write("Использование: wash.py json\n")
        raise SystemExit(2)
    data = query_wash_stations()
    sys.stdout.write(json.dumps(data, ensure_ascii=False))


if __name__ == "__main__":
    main()

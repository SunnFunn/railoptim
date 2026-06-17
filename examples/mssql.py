"""Подключение к MSSQL — те же переменные окружения, что src/data/dislocations.py."""

from __future__ import annotations

import os
import sys
from typing import Any

try:
    import pymssql
except ImportError:
    pymssql = None  # type: ignore[assignment]

MSSQL_ENV = (
    "MSSQL_SERVER_MSKASUVPL",
    "DOMAIN_USER",
    "PASSWORD",
    "MSSQL_DB_ASUVP",
    "MSSQL_DOMAIN",
)


def env(key: str, default: str | None = None) -> str | None:
    v = os.environ.get(key)
    if v is None or v == "":
        return default
    return v


def mssql_connect() -> Any:
    """Возвращает pymssql connection или завершает процесс с кодом 1."""
    if pymssql is None:
        print(
            "fetch_nsi: нужен pymssql (тот же пакет, что для src/data/dislocations.py)",
            file=sys.stderr,
        )
        sys.exit(1)

    server = env("MSSQL_SERVER_MSKASUVPL")
    if not server:
        print(
            "fetch_nsi: задайте MSSQL_SERVER_MSKASUVPL (секрет Infisical, как для dislocations.py)",
            file=sys.stderr,
        )
        sys.exit(1)

    user = env("DOMAIN_USER", "") or ""
    password = env("PASSWORD", "") or ""
    database = env("MSSQL_DB_ASUVP", "") or ""
    domain = env("MSSQL_DOMAIN", "") or ""

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

NSI_RAILWAY_PREFIX_SQL = """
SELECT S.Code6, R.ShortName, R.Name
FROM NSI.Station S (NOLOCK)
JOIN NSI.RailWay R (NOLOCK) ON S.RailWayId = R.RailWayId
WHERE S.Code6 IS NOT NULL
"""


def fetch_nsi_station_rows() -> list[tuple[Any, Any, Any]]:
    conn = mssql_connect()
    cur = conn.cursor()
    try:
        cur.execute(NSI_STATION_SQL)
        return list(cur.fetchall())
    finally:
        cur.close()
        conn.close()


def fetch_nsi_railway_prefix_rows() -> list[tuple[Any, Any, Any]]:
    """Строки для построения esr_prefixes: (Code6, ShortName, RailWayName)."""
    conn = mssql_connect()
    cur = conn.cursor()
    try:
        cur.execute(NSI_RAILWAY_PREFIX_SQL)
        return list(cur.fetchall())
    finally:
        cur.close()
        conn.close()


FIRM_SQL = """
SELECT
    F.FirmId,
    F.Name,
    F.ShortName,
    F.CodeOKPO,
    F.CodeINN,
    F.CodeKPP,
    F.CodeOGRN,
    F.CodeBIN,
    F.CreatedDateTime,
    F.UpdatedDateTime
FROM dbo.Firm F (NOLOCK)
WHERE NULLIF(LTRIM(RTRIM(F.CodeOKPO)), '') IS NOT NULL
   OR NULLIF(LTRIM(RTRIM(F.CodeINN)), '') IS NOT NULL
   OR NULLIF(LTRIM(RTRIM(F.CodeKPP)), '') IS NOT NULL
   OR NULLIF(LTRIM(RTRIM(F.CodeOGRN)), '') IS NOT NULL
   OR NULLIF(LTRIM(RTRIM(F.CodeBIN)), '') IS NOT NULL
"""


def fetch_firm_rows() -> list[tuple[Any, ...]]:
    conn = mssql_connect()
    cur = conn.cursor()
    try:
        cur.execute(FIRM_SQL)
        return list(cur.fetchall())
    finally:
        cur.close()
        conn.close()
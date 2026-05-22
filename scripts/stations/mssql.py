"""Подключение к MSSQL (те же переменные, что dislocations.py + generic MSSQL_*)."""

from __future__ import annotations

import os
import sys
from typing import Any

try:
    import pymssql
except ImportError:
    pymssql = None  # type: ignore[assignment]


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

    server = env("MSSQL_SERVER") or env("MSSQL_HOST") or env("MSSQL_SERVER_MSKASUVPL")
    if not server:
        print(
            "fetch_nsi: задайте MSSQL_SERVER, MSSQL_HOST или MSSQL_SERVER_MSKASUVPL",
            file=sys.stderr,
        )
        sys.exit(1)

    user = env("MSSQL_USER") or env("DOMAIN_USER") or ""
    password = env("MSSQL_PASSWORD") or env("PASSWORD") or ""
    database = env("MSSQL_DATABASE") or env("MSSQL_DB_ASUVP") or ""
    domain = env("MSSQL_DOMAIN") or ""

    return pymssql.connect(
        server=server,
        user=domain + user,
        password=password,
        database=database,
    )


NSI_STATION_SQL = "SELECT Code6, Name FROM NSI.Station (NOLOCK)"


def fetch_nsi_station_rows() -> list[tuple[Any, Any]]:
    conn = mssql_connect()
    cur = conn.cursor()
    try:
        cur.execute(NSI_STATION_SQL)
        return list(cur.fetchall())
    finally:
        cur.close()
        conn.close()

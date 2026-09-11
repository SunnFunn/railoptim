#!/usr/bin/env python3
"""Согласованные РЖД заявки ГУ-12 (SLP, MSSQL) → JSON в stdout для Rust.

  python3 gu12.py json [--periods 0:4,5:7,8:9,10:14]

Запускается из основного бинарника `railoptim` (см. src/data/gu12.rs) сразу после
загрузки узлов спроса. Тело запроса — согласованный с SLP запрос к vClaimGU12 +
график подач vClaimGu12OtprGraphPod (см. build_sql); окна периодов погрузки передаются
параметром `--periods` как смещения суток от текущей даты (включительно), чтобы
они **совпадали** с периодами спроса АПИ (`demand::DEMAND_PERIODS`). Без параметра
используются окна из примера: 1:5,6:8,9:10,11:15.

Переменные окружения (как у free_loadroads.py / dislocations.py):
  MSSQL_SERVER_MSKASUVPL, MSSQL_DB_SLP, DOMAIN_USER, PASSWORD, MSSQL_DOMAIN (опц.)

Stdout — JSON-массив строк:
  {
    "ClaimNumber":   "0000549300",
    "LoaderName":    "ООО \"Элеватор\"",   # грузоотправитель на станции погрузки
    "LoaderOkpo":    "00335717" | "",
    "StationCode":   "583506",             # код ЕСР-6 станции погрузки
    "StationName":   "КАЛАЧ",
    "StationToCode": "521001",
    "EtsngCode":     "011005",
    "Etsng":         "ПШЕНИЦА",
    "SendKind":      "Повагонная",
    "FinishDate":    "2026-09-30",
    "TotalCars":     36,
    "Cars":          [5, 3, 2, 5]          # вагонов по периодам 1..4
  }
Stderr — одна строка статистики: GU12_STATS=<json>.
"""

from __future__ import annotations

import json
import os
import sys
from typing import Any

try:
    import pymssql
except ImportError:  # pragma: no cover
    pymssql = None  # type: ignore[assignment]


DEFAULT_PERIODS = "1:5,6:8,9:10,11:15"

# Статусы согласования, при которых заявка считается разрешённой РЖД (как в gu12.sql).
AGREED_STATES = (
    "Согласована",
    "Согласована частично",
    "Согласована 53ф",
    "Согласована с изменениями 53ф",
)


def _env(key: str, default: str | None = None) -> str | None:
    v = os.environ.get(key)
    if v is None or v == "":
        return default
    return v


def _connect() -> Any:
    """Подключение к MSSQL (БД SLP) через pymssql; секреты — из окружения."""
    if pymssql is None:
        print("gu12: нужен pymssql (как для dislocations.py)", file=sys.stderr)
        sys.exit(1)

    server = _env("MSSQL_SERVER_MSKASUVPL")
    if not server:
        print("gu12: задайте MSSQL_SERVER_MSKASUVPL (секрет Infisical)", file=sys.stderr)
        sys.exit(1)
    database = _env("MSSQL_DB_SLP", "") or ""
    if not database:
        print("gu12: не задана БД MSSQL_DB_SLP", file=sys.stderr)
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


def parse_periods(spec: str) -> list[tuple[int, int]]:
    """`"0:4,5:7,8:9,10:14"` → `[(0, 4), (5, 7), (8, 9), (10, 14)]` (ровно 4 окна)."""
    out: list[tuple[int, int]] = []
    for part in spec.split(","):
        a, b = part.strip().split(":")
        lo, hi = int(a), int(b)
        if lo < 0 or hi < lo or hi > 60:
            raise ValueError(f"некорректное окно периода: {part!r}")
        out.append((lo, hi))
    if len(out) != 4:
        raise ValueError(f"ожидается 4 периода, получено {len(out)}")
    return out


def build_sql(periods: list[tuple[int, int]]) -> str:
    """SQL к vClaimGU12 с окнами периодов по смещениям суток (только целые — не пользовательский ввод)."""
    horizon_lo = min(lo for lo, _ in periods)
    horizon_hi = max(hi for _, hi in periods)

    def window(lo: int, hi: int, alias: str) -> str:
        return (
            "    SUM(CASE\n"
            f"            WHEN POD.PodDate >= DATEADD(day, {lo}, CAST(GETDATE() AS DATE))\n"
            f"             AND POD.PodDate <= DATEADD(day, {hi}, CAST(GETDATE() AS DATE))\n"
            "            THEN POD.CarCount\n"
            "            ELSE 0\n"
            f"        END) AS [{alias}]"
        )

    period_cols = ",\n".join(
        window(lo, hi, f"P{i + 1}") for i, (lo, hi) in enumerate(periods)
    )
    states = ", ".join(f"'{s}'" for s in AGREED_STATES)
    return f"""
SELECT
    GU.ClaimNumberInt,
    GU.LoaderFromName,
    GU.LoaderFromOKPO,
    GU.StationFromName,
    GU.StationFromCode6,
    GU.StationToName,
    GU.StationToCode6,
    GU.SendKindName,
    GU.FrETSNG,
    GU.FrETSNGCode6,
    GU.FinishDate,
    GU.CarCount AS TotalCars,
{period_cols}
FROM vClaimGU12 GU (NOLOCK)
JOIN vClaimGu12OtprGraphPod POD (NOLOCK) ON POD.ClaimGu12OtprId = GU.Id
WHERE
    POD.PodDate >= DATEADD(day, {horizon_lo}, CAST(GETDATE() AS DATE))
    AND POD.PodDate <= DATEADD(day, {horizon_hi}, CAST(GETDATE() AS DATE))
    AND GU.IsVisible = 1
    AND GU.StateName IN ({states})
    AND GU.FrETSNGCode6 LIKE '[05]%' -- зерновые: код ЕТСНГ начинается с 0 или 5
GROUP BY
    GU.ClaimNumberInt,
    GU.LoaderFromName,
    GU.LoaderFromOKPO,
    GU.StationFromName,
    GU.StationFromCode6,
    GU.StationToName,
    GU.StationToCode6,
    GU.SendKindName,
    GU.FrETSNG,
    GU.FrETSNGCode6,
    GU.FinishDate,
    GU.CarCount;
"""


def _s(v: Any) -> str:
    return "" if v is None else str(v).strip()


def _i(v: Any) -> int:
    try:
        return int(v) if v is not None else 0
    except (TypeError, ValueError):
        return 0


def _date(v: Any) -> str:
    if v is None:
        return ""
    if hasattr(v, "date"):
        try:
            return v.date().isoformat()
        except Exception:  # pragma: no cover
            pass
    if hasattr(v, "isoformat"):
        return v.isoformat()
    return str(v)[:10]


def fetch_gu12(periods: list[tuple[int, int]]) -> list[dict[str, Any]]:
    conn = _connect()
    cur = conn.cursor()
    try:
        cur.execute(build_sql(periods))
        rows = cur.fetchall()
    finally:
        cur.close()
        conn.close()

    out: list[dict[str, Any]] = []
    for r in rows:
        out.append(
            {
                "ClaimNumber":   _s(r[0]),
                "LoaderName":    _s(r[1]),
                "LoaderOkpo":    _s(r[2]),
                "StationName":   _s(r[3]),
                "StationCode":   _s(r[4]),
                "StationToName": _s(r[5]),
                "StationToCode": _s(r[6]),
                "SendKind":      _s(r[7]),
                "Etsng":         _s(r[8]),
                "EtsngCode":     _s(r[9]),
                "FinishDate":    _date(r[10]),
                "TotalCars":     _i(r[11]),
                "Cars":          [_i(r[12]), _i(r[13]), _i(r[14]), _i(r[15])],
            }
        )
    out.sort(key=lambda x: (x["StationCode"], x["LoaderOkpo"], x["LoaderName"], x["ClaimNumber"]))
    return out


def main(argv: list[str]) -> int:
    if len(argv) < 2 or argv[1] != "json":
        sys.stderr.write(f"Использование: gu12.py json [--periods {DEFAULT_PERIODS}]\n")
        return 2
    spec = DEFAULT_PERIODS
    if "--periods" in argv:
        i = argv.index("--periods")
        if i + 1 >= len(argv):
            sys.stderr.write("gu12: --periods требует значение\n")
            return 2
        spec = argv[i + 1]
    try:
        periods = parse_periods(spec)
    except ValueError as e:
        sys.stderr.write(f"gu12: {e}\n")
        return 2

    data = fetch_gu12(periods)
    stats = {
        "periods": [f"{lo}:{hi}" for lo, hi in periods],
        "rows": len(data),
        "stations": len({d["StationCode"] for d in data}),
        "rows_without_okpo": sum(1 for d in data if not d["LoaderOkpo"]),
        "cars_by_period": [sum(d["Cars"][k] for d in data) for k in range(4)],
    }
    print(f"GU12_STATS={json.dumps(stats, ensure_ascii=False)}", file=sys.stderr)

    json.dump(data, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

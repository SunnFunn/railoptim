#!/usr/bin/env python3
"""
Дислокация вагонов для периода 2–10 суток:
  1) Redis: hash `supply_data` — ключ = номер вагона, value = JSON; в список попадают только
     вагоны, у которых в value поле `OPZperiod` равно 10 (период 2–10 суток).
  2) MSSQL (pymssql): выборка по отобранным номерам.

Переменные окружения — Redis:
  REDIS_SUPPLY_HOST   (если не задан — печатается [] и выход 0)
  REDIS_SUPPLY_PORT   (по умолчанию 6380)
  REDIS_SUPPLY_DB     (по умолчанию 0)
  REDIS_SUPPLY_PASS   (опционально)

MSSQL (если задан REDIS_SUPPLY_HOST и в Redis есть ключи):
  MSSQL_SERVER или MSSQL_HOST
  MSSQL_USER
  MSSQL_PASSWORD
  MSSQL_DATABASE
  MSSQL_DOMAIN      (опционально, префикс к логину)

Вывод: JSON-массив объектов в формате полей NumberedCarItem (как в ответе АПИ railoptim).

Коды дорог в запросе: DP.FromRailwayCode, DP.ToRailwayCode. При других именах колонок в БД
используйте алиасы, например: YourCol AS FromRailWayCode.
"""

from __future__ import annotations

import json
import os
import sys
import fire

import pymssql
import redis


def _env(key: str, default: str | None = None) -> str | None:
    v = os.environ.get(key)
    if v is None or v == "":
        return default
    return v


def _car_type(capacity, volume) -> str:
    try:
        c = float(capacity or 0)
        v = float(volume or 0)
    except (TypeError, ValueError):
        return "Прочие"
    if v < 108.0 and c < 70.0:
        return "Прочие"
    if v >= 108.0 and c < 75.0:
        return "БК"
    if v < 108.0 and c >= 75.0:
        return "Т"
    return "БКТ"


def _to_int_opt(x):
    if x is None:
        return None
    try:
        return int(x)
    except (TypeError, ValueError):
        return None


def _to_float_opt(x):
    if x is None:
        return None
    try:
        return float(x)
    except (TypeError, ValueError):
        return None


def _is_opz_period_10(raw: str | None) -> bool:
    """
    В value поля hash `supply_data` ожидается JSON с полем OPZperiod.
    Учитывается только период 10 (10-суточное предложение).
    """
    if raw is None or (isinstance(raw, str) and not raw.strip()):
        return False
    try:
        obj = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return False
    if not isinstance(obj, dict):
        return False
    period = obj.get("OPZperiod")
    if period is None:
        period = obj.get("opzperiod")
    if period is None:
        return False
    try:
        return int(period) == 10
    except (TypeError, ValueError):
        return str(period).strip() == "10"


def _wagon_numbers_for_supply_period_10(r: redis.Redis) -> list[int]:
    """Номера вагонов из supply_data, у которых в JSON value OPZperiod == 10."""
    numbers: list[int] = []
    for field, raw in r.hgetall("supply_data").items():
        try:
            n = int(str(field).strip())
        except ValueError:
            continue
        if _is_opz_period_10(raw):
            numbers.append(n)
    return numbers


def _row_to_item(row: tuple) -> dict:
    """Порядок полей совпадает с SELECT в запросе."""
    (
        car_number,
        from_rw_part,
        from_rw,
        from_rw_code,
        st_from_name,
        st_from_code,
        to_rw_part,
        to_rw,
        to_rw_code,
        st_to_name,
        st_to_code,
        car_capacity,
        car_body_volume,
        _car_size,
        is_car_repair,
        car_next_repair_days,
        fr_etsng_code,
        fr_etsng_name,
        code6,
        prev_fr_etsng_name,
        grpo_name,
    ) = row

    ct = _car_type(car_capacity, car_body_volume)
    repair_days = _to_float_opt(car_next_repair_days)

    return {
        "CarNumber": int(car_number),
        "StationFrom": st_from_name,
        "StationFromCode": str(st_from_code).strip() if st_from_code is not None else None,
        "RailWayFromShort": from_rw,
        "RailWayFromCode": _to_int_opt(from_rw_code),
        "RailWayPartFrom": from_rw_part,
        "StationTo": st_to_name,
        "StationToCode": str(st_to_code).strip() if st_to_code is not None else None,
        "RailWayToShort": to_rw,
        "RailWayToCode": _to_int_opt(to_rw_code),
        "RailWayPartTo": to_rw_part,
        "OPZRailWayId": None,
        "OPZComment1": ct,
        "GRPOName": grpo_name,
        "FrETSNGCode": str(fr_etsng_code).strip() if fr_etsng_code is not None else None,
        "FrETSNGName": fr_etsng_name,
        "PrevFrETSNGCode": str(code6).strip() if code6 is not None else None,
        "PrevFrETSNGName": prev_fr_etsng_name,
        "CarNextRepairDays": repair_days,
        "IsCarRepair": bool(is_car_repair) if is_car_repair is not None else False,
    }


def main() -> None:
    host = _env("REDIS_SUPPLY_HOST")
    if not host:
        print("[]", flush=True)
        return

    port = int(_env("REDIS_SUPPLY_PORT", "6380") or "6380")
    db = int(_env("REDIS_SUPPLY_DB", "0") or "0")
    password = _env("REDIS_SUPPLY_PASS")

    r = redis.Redis(
        host=host,
        port=port,
        db=db,
        password=password,
        decode_responses=True,
    )
    numbers = _wagon_numbers_for_supply_period_10(r)
    if not numbers:
        print("[]", flush=True)
        return

    server = _env("MSSQL_SERVER_MSKASUVPL")
    if not server:
        print(
            "dislocations: задан REDIS_SUPPLY_HOST и есть ключи, но не задан MSSQL_SERVER",
            file=sys.stderr,
        )
        sys.exit(1)

    user = _env("DOMAIN_USER", "") or ""
    pw = _env("PASSWORD", "") or ""
    database = _env("MSSQL_DB_ASUVP", "") or ""
    domain = _env("MSSQL_DOMAIN", "") or ""

    conn = pymssql.connect(
        server=server,
        user=domain + user,
        password=pw,
        database=database,
    )

    sql_template = """
    SELECT
        DP.CarNumber,
        DP.FromRailWayPart, DP.FromRailWay, DP.FromRailwayCode, DP.StationFromName, DP.StationFromCode,
        DP.ToRailWayPart, DP.ToRailWay, DP.ToRailwayCode, DP.StationToName, DP.StationToCode,
        DP.CarCapacity, DP.CarBodyVolume, DP.CarSize, DP.IsCarRepair, DP.CarNextRepairDays,
        DP.FrETSNGCode, DP.FrETSNGName, FR.Code6, DP.PrevFrETSNGName,
        DP.GRPOName, DP.ShipmentGoalId
    FROM DislocationPreview DP (NOLOCK)
        JOIN NSI.FrETSNG FR ON FR.Name = DP.PrevFrETSNGName
        JOIN dynamic.CarComment CC (NOLOCK) ON CC.CarId = DP.CarId
        LEFT JOIN NSI_ETRAN.ShipmentGoal SG (NOLOCK) ON DP.TranspPurpose = SG.NAME
    WHERE DP.BelongType IN (N'Арендованный', N'В лизинге', N'Собственный')
        AND DP.CarKindName = N'Зерновозы'
        AND DP.CarNumber IN ({})
    """

    out: list[dict] = []
    batch_size = 400
    cur = conn.cursor()
    try:
        for i in range(0, len(numbers), batch_size):
            chunk = numbers[i : i + batch_size]
            in_list = ",".join(str(n) for n in chunk)
            cur.execute(sql_template.format(in_list))
            for row in cur.fetchall():
                out.append(_row_to_item(row))
    finally:
        cur.close()
        conn.close()

    print(json.dumps(out, ensure_ascii=False), flush=True)


# if __name__ == "__main__":
#     try:
#         main()
#     except Exception as exc:  # noqa: BLE001
#         print(f"dislocations.py: {exc}", file=sys.stderr)
#         sys.exit(1)

if __name__ == "__main__":
    # Fire автоматически обработает ошибки и выведет их в stderr
    fire.Fire(main)

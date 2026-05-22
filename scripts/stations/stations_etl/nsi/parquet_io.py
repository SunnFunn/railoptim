"""CSV/parquet I/O для этапа NSI."""

from __future__ import annotations

import csv
from pathlib import Path
from typing import Any

from stations_etl.nsi.process import NsiStationRecord


def load_csv_rows(path: Path) -> list[tuple[str, str]]:
    rows: list[tuple[str, str]] = []
    with path.open(encoding="utf-8-sig", newline="") as f:
        reader = csv.DictReader(f)
        if not reader.fieldnames:
            raise SystemExit(f"пустой CSV: {path}")
        fields = {h.strip().lower(): h for h in reader.fieldnames}
        code_key = fields.get("code6")
        name_key = fields.get("name")
        if not code_key or not name_key:
            raise SystemExit(f"CSV {path}: нужны колонки Code6 и Name")
        for row in reader:
            rows.append((row.get(code_key, ""), row.get(name_key, "")))
    return rows


def write_parquet(records: list[NsiStationRecord], path: Path) -> None:
    try:
        import pyarrow as pa
        import pyarrow.parquet as pq
    except ImportError as e:
        raise SystemExit(
            "fetch_nsi: установите pyarrow (scripts/stations/requirements-stations.txt)"
        ) from e

    path.parent.mkdir(parents=True, exist_ok=True)
    table = pa.Table.from_pylist([r.as_dict() for r in records])
    pq.write_table(table, path, compression="zstd")

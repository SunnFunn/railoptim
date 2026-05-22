"""Обработка сырых строк NSI.Station → parquet-ready записи + отчёт."""

from __future__ import annotations

import re
from collections import Counter, defaultdict
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any

from country import EsrCountryIndex
from normalize import normalize_esr6, validate_esr6_checksum

_WHITESPACE = re.compile(r"\s+")


def normalize_name(raw: Any) -> str:
    if raw is None:
        return ""
    s = str(raw).strip()
    return _WHITESPACE.sub(" ", s)


def _pick_name(names: list[str]) -> str:
    non_empty = [n for n in names if n]
    if not non_empty:
        return ""
    return max(non_empty, key=lambda n: (len(n), n))


@dataclass
class NsiStationRecord:
    esr6: str
    name_nsi: str
    code6_raw: str
    country_hint: str
    region_group: str
    network_district: str
    checksum_valid: bool

    def as_dict(self) -> dict[str, Any]:
        return {
            "esr6": self.esr6,
            "name_nsi": self.name_nsi,
            "code6_raw": self.code6_raw,
            "country_hint": self.country_hint,
            "region_group": self.region_group,
            "network_district": self.network_district,
            "checksum_valid": self.checksum_valid,
        }


def process_nsi_rows(
    rows: list[tuple[Any, Any]],
    country_index: EsrCountryIndex,
    *,
    source: str = "mssql",
) -> tuple[list[NsiStationRecord], dict[str, Any]]:
    rejected: list[dict[str, str]] = []
    by_esr6_names: dict[str, list[str]] = defaultdict(list)
    raw_by_esr6: dict[str, str] = {}

    for code6_raw, name_raw in rows:
        raw_str = "" if code6_raw is None else str(code6_raw).strip()
        name = normalize_name(name_raw)
        esr6 = normalize_esr6(code6_raw)
        if len(esr6) != 6 or not esr6.isdigit():
            rejected.append(
                {
                    "code6_raw": raw_str,
                    "name_nsi": name,
                    "reason": "invalid_esr6",
                }
            )
            continue
        by_esr6_names[esr6].append(name)
        if esr6 not in raw_by_esr6 and raw_str:
            raw_by_esr6[esr6] = raw_str

    duplicate_entries: list[dict[str, Any]] = []
    records: list[NsiStationRecord] = []

    for esr6 in sorted(by_esr6_names):
        names = by_esr6_names[esr6]
        unique_names = sorted(set(names))
        chosen = _pick_name(names)
        if len(unique_names) > 1:
            duplicate_entries.append(
                {
                    "esr6": esr6,
                    "names": unique_names,
                    "chosen": chosen,
                    "row_count": len(names),
                }
            )
        cls = country_index.classify(esr6)
        assert cls is not None
        records.append(
            NsiStationRecord(
                esr6=esr6,
                name_nsi=chosen,
                code6_raw=raw_by_esr6.get(esr6, esr6),
                country_hint=cls.country_hint,
                region_group=cls.region_group,
                network_district=cls.network_district,
                checksum_valid=validate_esr6_checksum(esr6),
            )
        )

    region_counts = Counter(r.region_group for r in records)
    checksum_invalid = sum(1 for r in records if not r.checksum_valid)

    report: dict[str, Any] = {
        "fetched_at": datetime.now(timezone.utc).isoformat(),
        "source": source,
        "nsi_total": len(rows),
        "nsi_unique_esr6": len(records),
        "nsi_rejected": len(rejected),
        "nsi_duplicate_esr6_count": len(duplicate_entries),
        "nsi_checksum_invalid": checksum_invalid,
        "nsi_by_region_group": dict(sorted(region_counts.items())),
        "nsi_duplicate_esr6": duplicate_entries[:500],
        "rejected_rows": rejected[:500],
    }
    if len(duplicate_entries) > 500:
        report["nsi_duplicate_esr6_truncated"] = len(duplicate_entries) - 500
    if len(rejected) > 500:
        report["rejected_rows_truncated"] = len(rejected) - 500

    return records, report

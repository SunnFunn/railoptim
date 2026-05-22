"""Классификация станции по префиксу сетевого района ЕСР (esr_country_prefixes.csv)."""

from __future__ import annotations

import csv
from dataclasses import dataclass
from pathlib import Path

from normalize import normalize_esr6

DEFAULT_PREFIXES_PATH = Path(__file__).resolve().parents[2] / "data/stations/esr_country_prefixes.csv"


@dataclass(frozen=True)
class EsrCountryRule:
    prefix_len: int
    prefix: str
    country_iso: str
    region_group: str
    note: str = ""


@dataclass(frozen=True)
class EsrClassification:
    esr6: str
    country_hint: str
    region_group: str
    network_district: str


class EsrCountryIndex:
    """Индекс правил по (prefix_len, prefix); неизвестный район → RU / ru."""

    def __init__(self, rules: list[EsrCountryRule], default_country: str = "RU", default_region: str = "ru"):
        self._rules: dict[tuple[int, str], EsrCountryRule] = {}
        for r in rules:
            self._rules[(r.prefix_len, r.prefix)] = r
        self._default_country = default_country
        self._default_region = default_region

    @classmethod
    def load(cls, path: Path | None = None) -> EsrCountryIndex:
        path = path or DEFAULT_PREFIXES_PATH
        rules: list[EsrCountryRule] = []
        with path.open(encoding="utf-8", newline="") as f:
            for row in csv.reader(f):
                if not row or row[0].startswith("#"):
                    continue
                if row[0].strip() == "prefix_len":
                    continue
                prefix_len = int(row[0])
                rules.append(
                    EsrCountryRule(
                        prefix_len=prefix_len,
                        prefix=row[1].strip(),
                        country_iso=row[2].strip(),
                        region_group=row[3].strip(),
                        note=row[4].strip() if len(row) > 4 else "",
                    )
                )
        return cls(rules)

    def classify(self, code: str | int | None) -> EsrClassification | None:
        esr6 = normalize_esr6(code)
        if len(esr6) != 6 or not esr6.isdigit():
            return None
        district = esr6[:2]
        rule = self._rules.get((2, district))
        if rule is None:
            return EsrClassification(
                esr6=esr6,
                country_hint=self._default_country,
                region_group=self._default_region,
                network_district=district,
            )
        return EsrClassification(
            esr6=esr6,
            country_hint=rule.country_iso,
            region_group=rule.region_group,
            network_district=district,
        )

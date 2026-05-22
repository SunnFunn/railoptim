#!/usr/bin/env python3
"""Unit-тесты парсинга ESR из OSM-тегов и merge (без PBF)."""

from __future__ import annotations

import sys

from stations_etl.osm.extract import (
    OsmEsrCandidate,
    iter_esr_from_tag_value,
    iter_esr_from_tags,
    merge_candidates,
)


class FakeTags(dict):
    def get(self, key, default=None):
        return super().get(key, default)


def test_iter_esr_from_tags() -> None:
    tags = FakeTags(
        {
            "railway": "station",
            "ref": "194013;532909",
            "uic_ref": "invalid",
            "name": "Test",
        }
    )
    pairs = list(iter_esr_from_tags(tags))
    assert pairs == [("ref", "194013"), ("ref", "532909")]

    tags2 = FakeTags({"esr:user": " 160001 "})
    assert list(iter_esr_from_tags(tags2)) == [("esr:user", "160001")]

    tags3 = FakeTags({"ref": "1234"})
    assert list(iter_esr_from_tags(tags3)) == [("ref", "001234")]


def test_iter_esr_split() -> None:
    assert list(iter_esr_from_tag_value("ref", "194013,532909")) == ["194013", "532909"]
    assert list(iter_esr_from_tag_value("ref", "abc")) == []


def test_merge_priority() -> None:
    low = OsmEsrCandidate(
        esr6="194013",
        lat=55.0,
        lon=37.0,
        osm_type="node",
        osm_id=1,
        tag_name="ref",
        name_osm="A",
        pbf_region="russia",
        pbf_priority=10,
        region_group="ru",
        railway="station",
        match_method="ref",
    )
    high = OsmEsrCandidate(
        esr6="194013",
        lat=55.1,
        lon=37.1,
        osm_type="node",
        osm_id=2,
        tag_name="ref",
        name_osm="B",
        pbf_region="other",
        pbf_priority=20,
        region_group="ru",
        railway="halt",
        match_method="ref",
    )
    merged = merge_candidates([low, high])
    assert merged.index["194013"].osm_id == 2
    assert merged.index["194013"].pbf_priority == 20


def main() -> int:
    test_iter_esr_from_tags()
    test_iter_esr_split()
    test_merge_priority()
    print("osm extract OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

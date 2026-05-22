# Справочник станций ЕСР + координаты

Пошаговая сборка (см. план в `.cursor/plans/`). **Пункт 1** (текущий):

| Файл | Назначение |
|------|------------|
| [`esr_country_prefixes.csv`](esr_country_prefixes.csv) | Сетевой район (2 цифры `Code6`) → `country_hint`, `region_group` |
| [`geofabrik_regions.yaml`](geofabrik_regions.yaml) | Манифест PBF для ETL OSM (пункт 3) |
| [`../../scripts/stations/normalize.py`](../../scripts/stations/normalize.py) | `normalize_esr6`, `validate_esr6_checksum` |
| [`../../scripts/stations/country.py`](../../scripts/stations/country.py) | `EsrCountryIndex::classify` |
| [`../../src/data/esr.rs`](../../src/data/esr.rs) | То же в Rust |

## Классификация зон

Группы отчётов: `ru`, `cis`, `baltic`, `china_mongolia`, `south_caucasus`.

Неизвестный **сетевой район** (первые 2 цифры не в CSV) → `RU` / `ru` (основная масса ~47k станций NSI).

Исключения ЕСР (район 74 = UZ/TJ/TM, Киргизия 71xx) описаны в комментариях CSV; после выгрузки NSI таблицу можно уточнить.

## Тесты пункта 1

```bash
# Python
cd scripts/stations && python3 run_parity_tests.py

# Rust
cargo test esr::
```

## Дальше (после подтверждения пункта 1)

2. `fetch_nsi_from_mssql.py` — выгрузка NSI.Station  
3. `build_osm_esr_index.py` — PBF по `geofabrik_regions.yaml`  
4. `build_stations_geo.py` — SQLite `stations_geo.sqlite`

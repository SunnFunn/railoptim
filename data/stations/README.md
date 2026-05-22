# Справочник станций ЕСР + координаты

Пошаговая сборка (см. план в `.cursor/plans/`).

## Пункт 1 — нормализация и классификация

| Файл | Назначение |
|------|------------|
| [`esr_country_prefixes.csv`](esr_country_prefixes.csv) | Сетевой район (2 цифры `Code6`) → `country_hint`, `region_group` |
| [`geofabrik_regions.yaml`](geofabrik_regions.yaml) | Манифест PBF для ETL OSM (пункт 3) |
| [`../../scripts/stations/normalize.py`](../../scripts/stations/normalize.py) | `normalize_esr6`, `validate_esr6_checksum` |
| [`../../scripts/stations/country.py`](../../scripts/stations/country.py) | `EsrCountryIndex::classify` |
| [`../../src/data/esr.rs`](../../src/data/esr.rs) | То же в Rust |

```bash
cd scripts/stations && python3 run_parity_tests.py
cargo test esr::
```

## Пункт 2 — выгрузка NSI.Station (MSSQL)

```bash
pip install -r scripts/stations/requirements-stations.txt

# Прод: переменные как у dislocations.py (Infisical / run.sh окружение)
python3 scripts/stations/fetch_nsi_from_mssql.py

# Тест без БД
cd scripts/stations && python3 run_nsi_tests.py
python3 fetch_nsi_from_mssql.py --input-csv test_nsi_sample.csv \
  --output /tmp/stations_nsi_raw.parquet --report /tmp/fetch_report.json
```

**Артефакты:**
- `data/stations/stations_nsi_raw.parquet` — `esr6`, `name_nsi`, `code6_raw`, `country_hint`, `region_group`, `network_district`, `checksum_valid`
- `data/stations/fetch_report.json` — `nsi_total`, `nsi_unique_esr6`, `nsi_by_region_group`, дубликаты, отклонённые строки

**MSSQL env:** `MSSQL_SERVER` / `MSSQL_HOST` / `MSSQL_SERVER_MSKASUVPL`, `MSSQL_USER` / `DOMAIN_USER`, `MSSQL_PASSWORD` / `PASSWORD`, `MSSQL_DATABASE` / `MSSQL_DB_ASUVP`, опционально `MSSQL_DOMAIN`.

## Дальше

3. `build_osm_esr_index.py` — PBF по `geofabrik_regions.yaml`  
4. `build_stations_geo.py` — SQLite `stations_geo.sqlite`

# Справочник станций ЕСР + координаты

Пошаговая сборка (см. план в `.cursor/plans/`).

## Скрипты запуска

| Скрипт | Сеть | Infisical | Назначение |
|--------|------|-----------|------------|
| [`fetch-nsi.sh`](../../scripts/stations/fetch-nsi.sh) | proxy-trap (как `run.sh`) | да | MSSQL → parquet |
| [`download-pbf.sh`](../../scripts/stations/download-pbf.sh) | **без** proxy-trap | нет | Geofabrik PBF (пункт 3) |
| [`run.sh`](../../scripts/stations/run.sh) | диспетчер | — | `fetch-nsi` / `download-pbf` / `test` |

На оффлайн-машине Infisical CLI без proxy-trap может пытаться выйти в интернет и упасть. **MSSQL-выгрузка** — только через `fetch-nsi.sh` (или `run.sh fetch-nsi`). **PBF** — отдельно, когда есть исходящий HTTPS.

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
pip install -r scripts/stations/requirements-stations.txt   # pyarrow

./scripts/stations/run.sh              # dev → fetch-nsi
./scripts/stations/run.sh prod

# или напрямую
./scripts/stations/fetch-nsi.sh prod

./scripts/stations/run.sh test
```

**Артефакты:** `data/stations/stations_nsi_raw.parquet`, `data/stations/fetch_report.json`

**MSSQL env (Infisical):** `MSSQL_SERVER_MSKASUVPL`, `DOMAIN_USER`, `PASSWORD`, `MSSQL_DB_ASUVP`, `MSSQL_DOMAIN` (опц.).

## Пункт 3 (далее) — OSM / PBF

```bash
./scripts/stations/run.sh download-pbf
# или ./scripts/stations/download-pbf.sh
```

## Пункт 4 (далее)

`build_stations_geo.py` — SQLite `stations_geo.sqlite`

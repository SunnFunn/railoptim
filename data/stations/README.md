# Справочник станций ЕСР + координаты

Пошаговая сборка ETL → [`stations_geo.sqlite`](stations_geo.sqlite) для карты и lookup в `railoptim`.

## Быстрый старт

```bash
pip install -r scripts/stations/requirements-stations.txt
# brew install libosmium   # macOS

# Полный pipeline (NSI → OSM → SQLite)
./scripts/stations/build_all.sh prod

# или по шагам — см. scripts/stations/run.sh
./scripts/stations/run.sh prod fetch-nsi      # proxy-trap + Infisical
./scripts/stations/run.sh download-pbf        # интернет
./scripts/stations/run.sh build-osm --index
./scripts/stations/run.sh build-geo           # оффлайн OK
```

Диспетчер: [`scripts/stations/run.sh`](../scripts/stations/run.sh) · полный pipeline: [`build_all.sh`](../scripts/stations/build_all.sh)

## Скрипты

| Скрипт | Сеть | Infisical | Назначение |
|--------|------|-----------|------------|
| [`fetch-nsi.sh`](../scripts/stations/fetch-nsi.sh) | proxy-trap | да | MSSQL → parquet |
| [`download-pbf.sh`](../scripts/stations/download-pbf.sh) | интернет | нет | скачать PBF |
| [`build-osm.sh`](../scripts/stations/build-osm.sh) | интернет | нет | PBF → OSM index |
| [`build-geo.sh`](../scripts/stations/build-geo.sh) | оффлайн | нет | NSI + OSM → SQLite |
| [`build_all.sh`](../scripts/stations/build_all.sh) | см. шаги | см. шаги | все три этапа |

`build_all.sh` options: `--skip-nsi`, `--skip-osm`, `--skip-geo`, `--include-optional`.

## Артефакты

| Файл | Описание |
|------|----------|
| `stations_nsi_raw.parquet` | NSI ~47k, `country_hint`, `region_group` |
| `osm_esr_index.parquet` | esr6 → lat/lon из OSM |
| `stations_geo.sqlite` | **продакшен** — только станции с coords |
| `build_report.json` | `coverage_by_region_group`, KPI |
| `unmatched_esr6.csv` | NSI без координат в OSM |

## Rust

[`src/data/stations_geo.rs`](../src/data/stations_geo.rs) — `STATIONS_GEO_DB` (default `data/stations/stations_geo.sqlite`), загрузка при старте `railoptim`.

```bash
cargo test stations_geo::
./scripts/stations/run.sh test
./scripts/stations/run.sh sample-geo --n 30
```

## Опционально (не реализовано)

Tier 2: [osm.sbin.ru/esr](http://osm.sbin.ru/esr/) (в основном РФ) · Tier 3: Wikidata P2815 для зарубежного хвоста.

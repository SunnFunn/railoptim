# Справочник станций ЕСР + координаты

Пошаговая сборка (см. план в `.cursor/plans/`).

## Скрипты запуска

| Скрипт | Сеть | Infisical | Назначение |
|--------|------|-----------|------------|
| [`fetch-nsi.sh`](../../scripts/stations/fetch-nsi.sh) | proxy-trap | да | MSSQL → parquet |
| [`download-pbf.sh`](../../scripts/stations/download-pbf.sh) | интернет | нет | только скачать PBF |
| [`build-osm.sh`](../../scripts/stations/build-osm.sh) | интернет | нет | PBF → `osm_esr_index.parquet` |
| [`run.sh`](../../scripts/stations/run.sh) | диспетчер | — | все команды ниже |

**Оффлайн-машина:** MSSQL — `fetch-nsi.sh` (proxy-trap + локальный Infisical). PBF/OSM — на хосте с HTTPS или перенос `data/stations/cache/pbf/` вручную + `build-osm.sh --index`.

## Зависимости

```bash
pip install -r scripts/stations/requirements-stations.txt
# osmium: нужен libosmium (macOS: brew install libosmium)
```

## Пункт 2 — NSI (MSSQL)

```bash
./scripts/stations/run.sh prod fetch-nsi
./scripts/stations/run.sh sample-nsi --n 30
```

Артефакты: `stations_nsi_raw.parquet`, `fetch_report.json`

## Пункт 3 — OSM / Geofabrik PBF

```bash
# Скачать все required PBF (~десятки GB суммарно; russia ~2.8 GB)
./scripts/stations/run.sh download-pbf

# Китай (optional, ~1.3 GB) + bbox при индексации
./scripts/stations/run.sh download-pbf --include-optional

# Только часть регионов (отладка)
./scripts/stations/run.sh download-pbf --regions belarus,latvia

# Индекс (download + extract, или --index если PBF уже в cache)
./scripts/stations/run.sh build-osm
./scripts/stations/run.sh build-osm --index          # без скачивания
./scripts/stations/run.sh build-osm --regions russia

# Визуальная проверка индекса
./scripts/stations/run.sh sample-osm --n 25
```

Арteфакты:
- `data/stations/cache/pbf/*.osm.pbf` — кэш Geofabrik
- `data/stations/osm_esr_index.parquet` — `esr6`, `lat`, `lon`, `pbf_region`, `match_method`, …
- `data/stations/osm_index_report.json` — статистика, ambiguous/cross_border

Теги OSM (приоритет): `ref` → `uic_ref` → `esr:user` → `railway:ref`. Объекты: `railway` ∈ {station, halt, stop}.

## Тесты

```bash
./scripts/stations/run.sh test
```

## Пункт 4 (далее)

`build_stations_geo.py` — join NSI + OSM → `stations_geo.sqlite`

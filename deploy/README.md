# railoptim-web — Axum API для карты назначений

Отдельный long-running сервис. Batch-оптимизация (`railoptim` + `run.sh`) работает независимо.

## Сборка

```bash
cd /path/to/railoptim
cargo build --release --bin railoptim-web
```

## Переменные окружения

| Переменная | Default | Описание |
|------------|---------|----------|
| `WEB_BIND_ADDR` | `0.0.0.0:8080` | Адрес HTTP-сервера |
| `STATIONS_GEO_DB` | `data/stations/stations_geo.sqlite` | SQLite справочник станций |
| `OPTIM_RESULT_DIR` | `tmp` | Каталог с `result_*.json` |
| `OPTIM_RESULT_FILE` | — | Явный путь к JSON (override latest) |
| `WEB_CORS_ORIGINS` | `*` | CORS origins через запятую |
| `RUST_LOG` | см. код | Фильтр tracing |

Web-сервер **не требует** `API_BASE_URL` / `API_TOKEN`.

## Запуск (dev)

```bash
export STATIONS_GEO_DB=data/stations/stations_geo.sqlite
export OPTIM_RESULT_FILE=tests/fixtures/optim_report_sample.json
cargo run --bin railoptim-web
```

## API (v1)

| Method | Path | Описание |
|--------|------|----------|
| GET | `/health` | Liveness |
| GET | `/api/v1/meta` | Версия, geo count, loaded plan |
| GET | `/api/v1/stations/{esr6}` | Lookup станции |
| GET | `/api/v1/plans` | Список `result_*.json` |
| GET | `/api/v1/plans/latest` | Последний план + OptimReport |
| GET | `/api/v1/plans/latest/map` | Данные для deck.gl (arcs + nodes) |
| POST | `/api/v1/plans/reload` | Перечитать JSON с диска |

## Связка с batch

После успешного cron `run.sh` появляется `tmp/result_YYYYMMDD_HHMMSS.json`.

Перезагрузить план в web без рестарта:

```bash
curl -X POST http://localhost:8080/api/v1/plans/reload
```

## systemd (Ubuntu prod)

Пример unit: [`deploy/railoptim-web.service`](railoptim-web.service)

```bash
sudo cp deploy/railoptim-web.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now railoptim-web
sudo systemctl status railoptim-web
```

Пути `User`, `WorkingDirectory`, `ExecStart` — подставить под prod.

## Smoke

```bash
curl -s localhost:8080/health
curl -s localhost:8080/api/v1/meta | jq .
curl -s localhost:8080/api/v1/plans/latest/map | jq '.stats'
```

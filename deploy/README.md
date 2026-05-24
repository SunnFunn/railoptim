# railoptim-web — Axum API для карты назначений

Отдельный long-running сервис. Batch-оптимизация (`railoptim` + `run.sh`) работает независимо.

## Prod: одна команда

Из корня репозитория на Ubuntu prod (пути в unit — см. [`systemd/railoptim-web.service`](systemd/railoptim-web.service)):

```bash
./deploy/install_web_service.sh
```

Скрипт:

1. `cargo build --release --bin railoptim --bin railoptim-web`
2. копирует бинарники в [`app/bin/`](../app/bin/) (`railoptim`, `railoptim-web`)
3. `ln -sf deploy/systemd/railoptim-web.service` → `/etc/systemd/system/`
4. `systemctl daemon-reload`, `enable`, `restart`

Prod batch (`./run.sh prod`) использует `app/bin/railoptim`.

Unit-файл **не копируется** — симлинк на репозиторий, правки в IDE сразу на месте.
После изменения unit: `sudo systemctl daemon-reload && sudo systemctl restart railoptim-web`.

Unit по умолчанию: `User=atretyakov`, `WorkingDirectory=/home/atretyakov/railoptim` — поправить при другом пути.
Wrapper [`start_web.sh`](start_web.sh) запускает `app/bin/railoptim-web`.

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
| POST | `/api/v1/plans/reload` | Перечитать JSON плана с диска |
| POST | `/api/v1/stations/reload` | Перечитать `stations_geo.sqlite` в память web |

## Связка с batch

После успешного cron `run.sh` появляется `tmp/result_YYYYMMDD_HHMMSS.json`.

Перезагрузить план в web без рестарта:

```bash
curl -X POST http://localhost:8080/api/v1/plans/reload
```

После `./scripts/stations/run.sh build-geo` — перечитать SQLite **без рестарта** (нужен актуальный `railoptim-web` с endpoint reload):

```bash
curl -X POST http://localhost:8080/api/v1/stations/reload
curl -s http://localhost:8080/api/v1/stations/521001 | jq .
```

Важно: `-X POST` обязателен. Обычный `curl …/stations/reload` (GET) вернёт **405** с подсказкой, а не перечитает каталог.

`./deploy/install_web_service.sh` **уже делает** `systemctl restart` в конце — отдельный рестарт после install не нужен. Рестарт нужен только при смене бинарника или unit-файла; для обновления данных geo после deploy достаточно `build-geo` + `POST …/stations/reload`.

## systemd (ручная установка)

Unit: [`deploy/systemd/railoptim-web.service`](systemd/railoptim-web.service)  
Рекомендуется: [`./deploy/install_web_service.sh`](install_web_service.sh)

```bash
sudo ln -sf /path/to/railoptim/deploy/systemd/railoptim-web.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now railoptim-web
```

Пути `User`, `Group`, `WorkingDirectory`, `ExecStart` в unit — под prod (см. [`systemd/railoptim-web.service`](systemd/railoptim-web.service)).

## Smoke

```bash
curl -s localhost:8080/health
curl -s localhost:8080/api/v1/meta | jq .
curl -s localhost:8080/api/v1/plans/latest/map | jq '.stats'
```

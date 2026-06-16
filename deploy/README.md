# railoptim-web — Axum API для карты назначений

Отдельный long-running сервис. Batch-оптимизация (`railoptim` + `run.sh`) работает независимо.

## Оффлайн prod (без npm)

На сервере **не нужен Node.js**: фронтенд `web-ui/dist` коммитится из dev-машины.

**Полная инструкция:** [`OFFLINE_PROD.md`](OFFLINE_PROD.md)

Кратко:

```bash
# Dev-машина (с npm):
./scripts/build_web_ui.sh && git add web-ui/dist && git push

# Оффлайн prod:
git pull && ./deploy/install_web_service.sh
```

## Prod: единый установщик `install.sh`

Из корня репозитория на Ubuntu prod (пути в unit — см. [`systemd/`](systemd/)):

```bash
./deploy/install.sh web      # frontend + railoptim-web (long-running сервис)
./deploy/install.sh optim    # batch railoptim + суточный timer (oneshot)
./deploy/install.sh all      # всё сразу
```

Скрипт по режиму:

1. (`web`) Проверяет `web-ui/dist/index.html` из git (или собирает через npm, если `REBUILD_WEB_UI=1`)
2. `cargo build --release` нужных бинарников (одним вызовом): `railoptim-web` для `web`, `railoptim` для `optim`, оба для `all`
3. копирует бинарники в [`app/bin/`](../app/bin/)
4. `ln -sf` нужных unit'ов → `/etc/systemd/system/`:
   - `web`   → `railoptim-web.service`
   - `optim` → `railoptim.service` + `railoptim.timer`
5. `systemctl daemon-reload`; затем `enable`+`restart railoptim-web` (web) и/или `enable --now railoptim.timer` (optim)

> Старые `install_web_service.sh` и `install_optim_services.sh` оставлены как тонкие обёртки (`install.sh web` / `install.sh optim`) для обратной совместимости.

Prod batch (`./run.sh prod`) использует `app/bin/railoptim` — его собирает режим `optim`/`all`.

Unit-файлы **не копируются** — симлинк на репозиторий, правки в IDE сразу на месте.
После изменения unit: `sudo systemctl daemon-reload` и `restart`/`enable --now` соответствующего unit.

## Суточный запуск batch-оптимизации (timer)

`railoptim.timer` запускает `railoptim.service` (`Type=oneshot` → `run.sh prod`) раз в сутки в **11:05**. В одном прогоне сначала собираются все данные и обновляется накопительная БД ёмкостей отстоя (`data/reserves/reserves.sqlite`, upsert по `etran_id`), затем запускается оптимизация, которая читает узлы отстоя уже из БД (с фильтром по истёкшим `date_end`).

Установка и запуск:

```bash
./deploy/install.sh optim     # соберёт app/bin/railoptim + поставит и включит timer
```

Проверка, что таймер активен и сервис отрабатывает:

```bash
systemctl list-timers 'railoptim*'         # NEXT/LAST — ближайший и прошлый запуск
systemctl status railoptim.timer           # active (waiting) — таймер взведён
systemctl status railoptim.service         # состояние последнего прогона (oneshot)
journalctl -u railoptim.service -n 100     # лог последнего прогона
```

Ручной прогон без ожидания таймера:

```bash
sudo systemctl start railoptim.service     # как по таймеру (run.sh prod, app/bin)
# или напрямую:
./run.sh prod                              # бинарник из app/bin
./run.sh dev                               # бинарник из target/release
```

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
| `WEB_STATIC_DIR` | `web-ui/dist` | SPA (deck.gl) |
| `WEB_MAP_DIR` | `data/map` | Оффлайн подложка: style, pmtiles, css |
| `RUST_LOG` | см. код | Фильтр tracing |

Web-сервер **не требует** `API_BASE_URL` / `API_TOKEN`.

## Frontend (web-ui)

SPA на React + deck.gl + MapLibre. В prod раздаётся с `:8080/` (тот же порт, что API).

**Dev** (два терминала):

```bash
# 1 — API
export STATIONS_GEO_DB=data/stations/stations_geo.sqlite
export OPTIM_RESULT_FILE=tests/fixtures/optim_report_sample.json
cargo run --bin railoptim-web

# 2 — Vite (proxy /api → :8080)
cd web-ui && npm install && npm run dev
```

Открыть http://localhost:5173

**Prod:** `./deploy/install_web_service.sh` собирает `web-ui/dist`. Smoke:

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8080/
curl -s http://127.0.0.1:8080/api/v1/plans/latest/map | jq '.stats, .filters'
```

Фильтры на карте: multi-select «дороги образования» / «дороги погрузки» — дуги между выбранными дорогами.

## Запуск API (dev, без frontend)

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
curl -s localhost:8080/api/v1/plans/latest/map | jq '.stats, .filters'
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8080/
```

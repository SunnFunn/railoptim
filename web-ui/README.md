# railoptim web-ui

SPA: карта назначений порожних вагонов (deck.gl + MapLibre).

## Оффлайн prod (без npm на сервере)

**Сборка только на машине с Node.js**, артефакт коммитится в git:

```bash
# из корня репозитория
./scripts/build_web_ui.sh
git add web-ui/dist
git commit -m "build(web-ui): обновить dist"
git push
```

На prod: `git pull` → `./deploy/install_web_service.sh` (или только `systemctl restart` если менялся только dist).

Подробно: [`deploy/OFFLINE_PROD.md`](../deploy/OFFLINE_PROD.md).

## Dev (локально)

```bash
# terminal 1 — API
export STATIONS_GEO_DB=../data/stations/stations_geo.sqlite
export OPTIM_RESULT_FILE=../tests/fixtures/optim_report_sample.json
cargo run --bin railoptim-web

# terminal 2 — hot reload UI
npm install
npm run dev
```

http://localhost:5173 — Vite проксирует `/api` на `:8080`.

## Prod (с npm на той же машине)

```bash
REBUILD_WEB_UI=1 ./deploy/install_web_service.sh
```

## Env (Vite)

| Variable | Default | Description |
|----------|---------|-------------|
| `VITE_API_BASE` | `` | API prefix (empty = same origin) |
| `VITE_MAP_STYLE` | OpenFreeMap liberty | MapLibre style URL |

## Что в git

| Путь | В git |
|------|-------|
| `web-ui/src/`, `package.json`, `package-lock.json` | да |
| `web-ui/dist/` (без `*.map`) | **да** — для оффлайн prod |
| `web-ui/node_modules/` | нет |

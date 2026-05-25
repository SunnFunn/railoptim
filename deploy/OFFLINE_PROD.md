# Оффлайн prod: деплой без npm

На prod-машине **нет Node.js/npm** — фронтенд (`web-ui/dist`) собирается на другой машине,
коммитится в GitHub, на prod подтягивается через `git pull`.

Rust-бинарники (`railoptim`, `railoptim-web`) **собираются на prod** через `cargo` (интернет
нужен только для crates.io при первой сборке; далее — кэш `target/`).

---

## Схема

```text
[Машина с npm]                    [GitHub]                 [Оффлайн prod]
  ./scripts/build_web_ui.sh  -->  git push   -->   git pull
  git add web-ui/dist                                    ./deploy/install_web_service.sh
                                                       cargo build + systemd
```

---

## Часть 1. Машина разработки (с npm)

Требования: **Node.js 20+**, **npm**, **git**, доступ к GitHub.

### 1.1 Сборка фронтенда

Из корня репозитория:

```bash
chmod +x scripts/build_web_ui.sh
./scripts/build_web_ui.sh
```

Скрипт создаёт/обновляет `web-ui/dist/` и `web-ui/dist/build-info.json` (дата, версия node, git rev).

### 1.2 Коммит и push

```bash
git add web-ui/dist web-ui/package-lock.json
git status   # убедитесь, что dist/*.js, dist/index.html в индексе
git commit -m "build(web-ui): обновить dist для оффлайн prod"
git push origin <ваша-ветка>
```

**В git попадает:** `web-ui/dist/` (без `*.map` — sourcemap в `.gitignore`).

**Не коммитить:** `web-ui/node_modules/`.

### 1.3 Когда пересобирать dist

После любых правок в `web-ui/src/`, `package.json`, `vite.config.ts` — снова
`./scripts/build_web_ui.sh` → commit → push.

---

## Часть 2. Оффлайн prod — первичная установка

Требования на сервере:

| Компонент | Назначение |
|-----------|------------|
| **git** | Клонирование/обновление репозитория |
| **Rust + cargo** | Сборка `railoptim`, `railoptim-web` |
| **sqlite3** | Проверка справочника станций (опционально) |
| **systemd** | Сервис `railoptim-web` |
| **Infisical CLI** | Секреты для batch (`run.sh prod`) — как уже настроено |
| **Нет npm** | Не требуется |

Интернет: доступ к **GitHub** и (для первой сборки Rust) **crates.io**.

### 2.1 Клонирование

```bash
cd ~
git clone git@github.com:<org>/railoptim.git
cd railoptim
```

Проверьте, что фронтенд в репозитории:

```bash
ls -la web-ui/dist/index.html
cat web-ui/dist/build-info.json
```

Если `index.html` нет — на dev-машине не закоммитили dist, см. часть 1.

### 2.2 Пути в systemd

Отредактируйте [`deploy/systemd/railoptim-web.service`](systemd/railoptim-web.service):

- `User`, `Group` — пользователь prod
- `WorkingDirectory` — полный путь к репозиторию, например `/home/atretyakov/railoptim`
- Пути в `Environment=` (`STATIONS_GEO_DB`, `WEB_STATIC_DIR`, `OPTIM_RESULT_DIR`)

По умолчанию:

```ini
WorkingDirectory=/home/atretyakov/railoptim
Environment="STATIONS_GEO_DB=/home/atretyakov/railoptim/data/stations/stations_geo.sqlite"
Environment="WEB_STATIC_DIR=/home/atretyakov/railoptim/web-ui/dist"
Environment="OPTIM_RESULT_DIR=/home/atretyakov/railoptim/tmp"
```

### 2.3 Справочник станций (geo)

`data/stations/stations_geo.sqlite` **не в git** — создаётся ETL на prod или копируется с другой машины:

```bash
# если ETL уже настроен на prod:
./scripts/stations/run.sh build-geo
# или скопировать готовый файл в data/stations/stations_geo.sqlite
```

### 2.4 Установка web-сервиса

```bash
cd ~/railoptim
./deploy/install_web_service.sh
```

Скрипт:

1. Проверяет `web-ui/dist/index.html` (из git)
2. `cargo build --release` → `app/bin/railoptim`, `app/bin/railoptim-web`
3. systemd enable + restart

**npm не вызывается**, если dist уже в репозитории.

### 2.5 План назначений для карты

Web читает последний `tmp/result_*.json` после batch-оптимизации.

Первый запуск batch (как у вас настроено):

```bash
./run.sh prod
```

Затем перечитать план в web (без рестарта):

```bash
curl -X POST http://127.0.0.1:8080/api/v1/plans/reload
```

### 2.6 Проверка

```bash
curl -s http://127.0.0.1:8080/health
curl -s http://127.0.0.1:8080/api/v1/meta | jq .
curl -s http://127.0.0.1:8080/api/v1/plans/latest/map | jq '.stats, .filters'
curl -s -o /dev/null -w 'SPA HTTP %{http_code}\n' http://127.0.0.1:8080/
```

В браузере: `http://<IP-сервера>:8080/` — карта с дугами назначений.

Логи:

```bash
journalctl -u railoptim-web -f
```

Ожидается строка `serving_spa=true` при старте.

---

## Часть 3. Обновление на оффлайн prod

### Только backend (Rust / deploy)

```bash
cd ~/railoptim
git pull
./deploy/install_web_service.sh
```

`web-ui/dist` не менился — пересборка npm не нужна.

### Только frontend (карта)

На **dev-машине**: `./scripts/build_web_ui.sh` → commit → push.

На **prod**:

```bash
cd ~/railoptim
git pull
sudo systemctl restart railoptim-web
```

Пересборка `cargo` не обязательна, если менялся только `web-ui/dist`.

### Данные geo или план

```bash
./scripts/stations/run.sh build-geo
curl -X POST http://127.0.0.1:8080/api/v1/stations/reload

./run.sh prod
curl -X POST http://127.0.0.1:8080/api/v1/plans/reload
```

---

## Часть 4. Устранение неполадок

| Симптом | Причина | Решение |
|---------|---------|---------|
| `ERROR: нет web-ui/dist/index.html` | dist не в git / не сделали pull | Часть 1 → `git pull` |
| `:8080/` 404, API работает | нет dist или `WEB_STATIC_DIR` неверный | Проверить путь в unit и `ls web-ui/dist/` |
| Карта пустая | нет `tmp/result_*.json` | `./run.sh prod` + `POST …/plans/reload` |
| Нет дуг на карте | нет координат в sqlite | `build-geo`, `POST …/stations/reload` |
| `serving_spa=false` в логах | нет index.html в `WEB_STATIC_DIR` | `git pull`, проверить dist |

### Принудительная пересборка UI на prod (если появится npm)

```bash
REBUILD_WEB_UI=1 ./deploy/install_web_service.sh
```

На оффлайн prod обычно **не используйте** — собирайте dist на dev-машине.

---

## Чеклист «полный проект на prod»

- [ ] `git clone` / `git pull` с веткой, где есть `web-ui/dist/`
- [ ] `data/stations/stations_geo.sqlite` на месте
- [ ] Пути в `deploy/systemd/railoptim-web.service` под пользователя и каталог
- [ ] `./deploy/install_web_service.sh` успешен
- [ ] `systemctl status railoptim-web` — active
- [ ] `curl /health` — ok
- [ ] `curl /` — HTTP 200
- [ ] После batch: `POST /api/v1/plans/reload`
- [ ] Карта в браузере отображает дуги

См. также: [`deploy/README.md`](README.md), [`web-ui/README.md`](../web-ui/README.md).

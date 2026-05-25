# Оффлайн подложка карты (Protomaps PMTiles)

Каталог для MapLibre basemap без openfreemap.org / unpkg.com.

## Файлы

| Файл | В git | Описание |
|------|-------|----------|
| `style.json` | да | MapLibre style (Protomaps light, **lang: ru**) |
| `maplibre-gl.css` | да | Стили MapLibre (копия из npm) |
| `glyphs/` | да | Шрифты `.pbf` |
| `sprites/v4/light/` | да | Спрайты POI |
| `ru_cis.pmtiles` | **нет** | Тайлы Россия+СНГ (большой файл) |
| `railways_zones.geojson` | да | Контуры зон ж/д (Supermap WFS `rworgs`, коды NSI) |
| `railways_zones_report.json` | нет | Отчёт импорта Supermap |
| `supermap_rw_name_to_rw.csv` | да | Маппинг «Октябрьская ЖД» → `ОКТ` |
| `supermap_rworgs_raw.geojson` | нет | Сырой WFS (опционально, после fetch) |
| `railways_voronoi.geojson` | устар. | Legacy Voronoi — не использовать |
| `download_manifest.json` | опционально | Результат verify |
| `verify_report.txt` | нет | Лог проверки |

## Этап 1: проверка скачивания (онлайн-машина)

```bash
chmod +x scripts/map/*.sh
./scripts/map/verify_offline_downloads.sh
```

### PMTiles RU+СНГ (обязательно для prod)

**Вариант A — extract по bbox (рекомендуется, ~8 GB, без скачивания всего planet):**

На машине с интернетом:

```bash
chmod +x scripts/map/download_ru_cis_pmtiles.sh
./scripts/map/download_ru_cis_pmtiles.sh
# dry-run оценки объёма: ./scripts/map/download_ru_cis_pmtiles.sh --dry-run
```

Скрипт качает только регион из дневного build (`PMTILES_PLANET_URL`, по умолчанию
`https://build.protomaps.com/YYYYMMDD.pmtiles`) через HTTP Range. Bbox: `19,35,180,82`, maxzoom **13**.

**Вариант B — вручную с build.protomaps.com:**

1. [build.protomaps.com](https://build.protomaps.com) — Russia + СНГ, max zoom 13–14
2. Сохранить как `data/map/ru_cis.pmtiles`

**Вариант C — готовый URL:**

```bash
export PMTILES_URL='https://...'
./scripts/map/verify_offline_downloads.sh
```

Проверка архива: `.tools/pmtiles show data/map/ru_cis.pmtiles`

### Язык подписей (ru / en)

Подписи городов задаёт **`style.json`**, не pmtiles. Генерация: `scripts/map/generate_style.mjs` (`lang: "ru"`).
После смены языка достаточно `./scripts/build_web_ui.sh` и `git add data/map/style.json` — **pmtiles перекачивать не нужно**.

### Smoke без полного RU (только тест)

```bash
DOWNLOAD_SAMPLE=1 ./scripts/map/verify_offline_downloads.sh
```

Скачает маленький Monaco sample (~3 MB), **не** для prod.

### Критерии go

- `maplibre-gl.css` есть
- `ru_cis.pmtiles` > 50 MB (или sample для dev)
- `glyphs/` не пустой

## Зоны ж/д дорог (Supermap)

Полигоны сетей с [Суперкарты 2.0](https://supermap.zatramvaj.su/) (WFS `Supermap_GeoServer:rworgs`),
фильтр по [`railway_rw_allowlist.txt`](railway_rw_allowlist.txt), коды — из
[`supermap_rw_name_to_rw.csv`](supermap_rw_name_to_rw.csv) (маппинг «Московская ЖД» → `МСК`).

```bash
cd scripts/map
uv sync   # опционально; fetch использует stdlib
./run.sh fetch-zones
# → data/map/railways_zones.geojson
# → data/map/railways_zones_report.json
```

Оффлайн prod: **`git pull`** готового `railways_zones.geojson` (интернет на prod не нужен).

Добавить дорогу: строка в `supermap_rw_name_to_rw.csv` + код в allowlist → пересборка `fetch-zones`.

**UI:** чекбокс «Зоны дорог (Supermap)», контуры одного цвета, подпись **3-буквенного кода** (`rw`).

```bash
curl -sI http://127.0.0.1:8080/map/railways_zones.geojson
```

### Legacy: Voronoi

`./run.sh build-voronoi` — устаревший расчёт по станциям; для prod не использовать.

## Подготовка артефактов в git

```bash
./scripts/map/copy_map_assets.sh
git add data/map/style.json data/map/maplibre-gl.css data/map/glyphs data/map/sprites \
  data/map/railway_rw_allowlist.txt data/map/supermap_rw_name_to_rw.csv data/map/railways_zones.geojson
```

## Доставка на оффлайн prod

```bash
rsync -avP data/map/ru_cis.pmtiles user@prod:~/railoptim/data/map/
```

На prod: `git pull` + `./deploy/install_web_service.sh`

## Smoke на prod

```bash
curl -sI http://127.0.0.1:8080/map/style.json
curl -sI http://127.0.0.1:8080/map/ru_cis.pmtiles | grep -i accept-ranges
curl -sI http://127.0.0.1:8080/map/railways_zones.geojson
```

В браузере F12 → Network: **нет** запросов к openfreemap.org, unpkg.com.

См. [`deploy/OFFLINE_PROD.md`](../deploy/OFFLINE_PROD.md).

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
| `railways_voronoi.geojson` | да | Пилот: контуры зон ж/д (Voronoi, 3-букв. коды) |
| `railways_voronoi_report.json` | нет | Отчёт сборки зон |
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

## Зоны ж/д дорог (Voronoi, пилот)

Условные границы сетей по станциям (не официальные полигоны РЖД). Подписи — **3-буквенные коды**
(КБШ, СКВ, ПРВ…) из `NSI.RailWay.ShortName`, fallback — [`data/stations/esr_district_to_rw.csv`](../stations/esr_district_to_rw.csv).

```bash
python3 -m venv .venv-map
.venv-map/bin/pip install -r scripts/map/requirements-map.txt

# после build-geo и fetch-nsi:
.venv-map/bin/python scripts/map/build_railway_voronoi.py
# отчёт: data/map/railways_voronoi_report.json
```

Параметры: `--region ru` (по умолчанию), `--bbox 19,35,180,82`, `--region all` для теста.

В UI: чекбокс «Зоны дорог (пилот)» — контуры без заливки, подпись в левом верхнем углу bbox зоны.

## Подготовка артефактов в git

```bash
./scripts/map/copy_map_assets.sh
git add data/map/style.json data/map/maplibre-gl.css data/map/glyphs data/map/sprites data/map/railways_voronoi.geojson
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
curl -sI http://127.0.0.1:8080/map/railways_voronoi.geojson
```

В браузере F12 → Network: **нет** запросов к openfreemap.org, unpkg.com.

См. [`deploy/OFFLINE_PROD.md`](../deploy/OFFLINE_PROD.md).

//! Справочник свободных ёмкостей подъездных путей крупных станций погрузки.
//!
//! Пересобирается каждый суточный прогон `railoptim`. Источники:
//!   * `data/load_stations.json` — ёмкость путей станции (`station_road_capacity`),
//!     имя, дорога, код ЕСР-6 (см. `src/bin/build_load_stations.rs`);
//!   * MSSQL через `src/data/free_loadroads.py`:
//!       - Шаг 1 (БД `MSSQL_DB_SLP`): станции погрузки зерна за последние 6 мес.
//!         (множество кодов ЕСР — «куда можно ставить вагоны»);
//!       - Шаг 2 (БД `MSSQL_DB_ASUVP`): число вагонов, уже стоящих на станции
//!         (`DislocationPreview.Distance = 0`).
//!
//! Итог (`data/load_stations_free_capacity.json`) — только крупные станции
//! (`station_road_capacity > LARGE_STATION_MIN_ROAD_CAPACITY`), присутствующие в
//! результатах Шага 1, с положительной свободной ёмкостью
//! (`FreeRailRoadCapacity > FREE_CAPACITY_MIN`).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::esr::normalize_esr6;

/// Путь к справочнику станций погрузки (вход).
pub const DEFAULT_LOAD_STATIONS_PATH: &str = "data/load_stations.json";
/// Путь к формируемому справочнику свободных ёмкостей путей (выход).
pub const DEFAULT_OUTPUT_PATH: &str = "data/load_stations_free_capacity.json";

/// Порог «крупной» станции: в справочник идут только станции с ёмкостью путей
/// строго больше этого значения (вагонов).
pub const LARGE_STATION_MIN_ROAD_CAPACITY: i64 = 50;
/// Порог отбора по свободной ёмкости: остаются станции, где свободно строго больше
/// этого числа вагонов (управляющая константа).
pub const FREE_CAPACITY_MIN: i64 = 20;

/// Запись справочника свободных ёмкостей путей (ключи JSON — как в постановке задачи).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreeLoadRoad {
    #[serde(rename = "LoadRoadName")]
    pub load_road_name: String,
    #[serde(rename = "LoadStationName")]
    pub load_station_name: String,
    #[serde(rename = "LoadStationCode")]
    pub load_station_code: String,
    #[serde(rename = "RailRoadCapacity")]
    pub rail_road_capacity: i64,
    #[serde(rename = "CarsOnRailRoads")]
    pub cars_on_rail_roads: i64,
    #[serde(rename = "FreeRailRoadCapacity")]
    pub free_rail_road_capacity: i64,
}

/// Строка `data/load_stations.json`.
#[derive(Debug, Clone, Deserialize)]
struct LoadStationRef {
    #[serde(default)]
    rail_road: String,
    #[serde(default)]
    load_station: String,
    #[serde(default)]
    load_station_code: Option<String>,
    #[serde(default)]
    station_road_capacity: i64,
}

/// Данные MSSQL от `free_loadroads.py`.
#[derive(Debug, Clone, Default, Deserialize)]
struct MssqlPayload {
    #[serde(default)]
    load_station_codes: Vec<String>,
    #[serde(default)]
    cars_on_station: HashMap<String, i64>,
}

/// Нормализованные данные MSSQL для расчёта.
#[derive(Debug, Clone, Default)]
struct MssqlData {
    load_station_codes: HashSet<String>,
    cars_on_station: HashMap<String, i64>,
}

/// Полный цикл: читает `load_stations.json`, тянет данные MSSQL, считает свободные
/// ёмкости и пишет `output_path`. Возвращает построенные записи.
pub fn build_free_loadroads(
    load_stations_path: impl AsRef<Path>,
    output_path: impl AsRef<Path>,
) -> Result<Vec<FreeLoadRoad>> {
    let stations = load_station_refs(load_stations_path.as_ref())?;
    let mssql = fetch_mssql_data()?;
    let records = compute_free_loadroads(&stations, &mssql);

    let json = serde_json::to_string_pretty(&records).context("сериализация справочника")?;
    let out = output_path.as_ref();
    std::fs::write(out, json.as_bytes())
        .with_context(|| format!("запись {}", out.display()))?;
    Ok(records)
}

fn load_station_refs(path: &Path) -> Result<Vec<LoadStationRef>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("чтение {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("разбор {}", path.display()))
}

/// Запускает `src/data/free_loadroads.py`, читает коды станций погрузки и вагоны на станциях.
fn fetch_mssql_data() -> Result<MssqlData> {
    let script = std::env::current_dir()
        .context("текущая директория")?
        .join("src/data/free_loadroads.py");
    let output = Command::new("python3")
        .arg(&script)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("запуск python3 {}", script.display()))?;

    if !output.status.success() {
        bail!("free_loadroads.py завершился с кодом {:?}", output.status.code());
    }

    let payload: MssqlPayload =
        serde_json::from_slice(&output.stdout).context("разбор JSON от free_loadroads.py")?;

    let load_station_codes = payload
        .load_station_codes
        .into_iter()
        .map(|c| normalize_esr6(&c))
        .filter(|c| c.len() == 6)
        .collect();
    let cars_on_station = payload
        .cars_on_station
        .into_iter()
        .filter_map(|(k, v)| {
            let code = normalize_esr6(&k);
            (code.len() == 6).then_some((code, v))
        })
        .collect();

    Ok(MssqlData { load_station_codes, cars_on_station })
}

/// Чистый расчёт (без БД): отбор крупных станций погрузки и их свободных ёмкостей.
fn compute_free_loadroads(stations: &[LoadStationRef], mssql: &MssqlData) -> Vec<FreeLoadRoad> {
    let mut out = Vec::new();
    for s in stations {
        // Только крупные станции (строго > порога).
        if s.station_road_capacity <= LARGE_STATION_MIN_ROAD_CAPACITY {
            continue;
        }
        let code = match &s.load_station_code {
            Some(c) => normalize_esr6(c),
            None => continue,
        };
        if code.len() != 6 {
            continue;
        }
        // Шаг 1: жёсткий фильтр — только реальные станции погрузки за 6 мес.
        if !mssql.load_station_codes.contains(&code) {
            continue;
        }
        let cars = mssql.cars_on_station.get(&code).copied().unwrap_or(0);
        let free = s.station_road_capacity - cars;
        // Только станции с заметной свободной ёмкостью (строго > порога).
        if free <= FREE_CAPACITY_MIN {
            continue;
        }
        out.push(FreeLoadRoad {
            load_road_name: s.rail_road.clone(),
            load_station_name: s.load_station.clone(),
            load_station_code: code,
            rail_road_capacity: s.station_road_capacity,
            cars_on_rail_roads: cars,
            free_rail_road_capacity: free,
        });
    }
    // По убыванию свободной ёмкости, затем по коду — детерминированный порядок.
    out.sort_by(|a, b| {
        b.free_rail_road_capacity
            .cmp(&a.free_rail_road_capacity)
            .then_with(|| a.load_station_code.cmp(&b.load_station_code))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(road: &str, name: &str, code: Option<&str>, cap: i64) -> LoadStationRef {
        LoadStationRef {
            rail_road: road.to_string(),
            load_station: name.to_string(),
            load_station_code: code.map(|c| c.to_string()),
            station_road_capacity: cap,
        }
    }

    fn mssql(codes: &[&str], cars: &[(&str, i64)]) -> MssqlData {
        MssqlData {
            load_station_codes: codes.iter().map(|c| c.to_string()).collect(),
            cars_on_station: cars.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }

    #[test]
    fn keeps_large_loading_station_with_free_capacity() {
        let stations = vec![st("ПРВ", "Большая", Some("612408"), 100)];
        let data = mssql(&["612408"], &[("612408", 30)]);
        let out = compute_free_loadroads(&stations, &data);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.load_station_code, "612408");
        assert_eq!(r.rail_road_capacity, 100);
        assert_eq!(r.cars_on_rail_roads, 30);
        assert_eq!(r.free_rail_road_capacity, 70);
    }

    #[test]
    fn drops_small_station_by_capacity() {
        // Ёмкость ровно 50 — не «крупная» (нужно строго > 50).
        let stations = vec![st("ПРВ", "Малая", Some("612408"), 50)];
        let data = mssql(&["612408"], &[]);
        assert!(compute_free_loadroads(&stations, &data).is_empty());
    }

    #[test]
    fn drops_station_absent_in_step1() {
        let stations = vec![st("ПРВ", "НеГрузовая", Some("612408"), 100)];
        let data = mssql(&[], &[]); // нет в Шаге 1
        assert!(compute_free_loadroads(&stations, &data).is_empty());
    }

    #[test]
    fn drops_station_with_low_free_capacity() {
        // free = 70 - 50 = 20, не строго > 20 → отбрасываем.
        let stations = vec![st("ПРВ", "ПочтиПолная", Some("612408"), 70)];
        let data = mssql(&["612408"], &[("612408", 50)]);
        assert!(compute_free_loadroads(&stations, &data).is_empty());
    }

    #[test]
    fn missing_cars_treated_as_zero() {
        let stations = vec![st("ПРВ", "БезВагонов", Some("612408"), 80)];
        let data = mssql(&["612408"], &[]); // в Шаге 2 нет записи
        let out = compute_free_loadroads(&stations, &data);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].cars_on_rail_roads, 0);
        assert_eq!(out[0].free_rail_road_capacity, 80);
    }

    #[test]
    fn skips_entries_without_code() {
        let stations = vec![st("ПРВ", "БезКода", None, 100)];
        let data = mssql(&["612408"], &[]);
        assert!(compute_free_loadroads(&stations, &data).is_empty());
    }
}

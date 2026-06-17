//! Генерация справочника станций погрузки `data/load_stations.json` из `data/LoadStations.xlsx`.
//!
//! Колонки Excel (1-based): D — станция погрузки, E — дорога (3 буквы),
//! AF — мощность подъездных путей (вагоны единовременно), AO — мощность погрузки в сутки.
//! Заголовок в строках 6–7, данные — строки 8..=1296. Одна станция может встречаться
//! несколько раз (разные грузоотправители/элеваторы) — мощности суммируются.
//!
//! Коды ЕСР-6 в Excel отсутствуют: они подбираются по имени станции + дороге в MSSQL
//! через самодостаточный Python-хелпер `src/data/load_stations_esr.py`.
//!
//! Запуск (из корня проекта, с окружением, где доступны секреты MSSQL_*):
//!   cargo run --bin railoptim-load-stations -- [--input data/LoadStations.xlsx]
//!                                              [--output data/load_stations.json]
//!                                              [--dry-run]   (без обращения к MSSQL, code=null)

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use calamine::{open_workbook_auto, Data, DataType, Reader};
use serde::{Deserialize, Serialize};

/// 1-based номера колонок Excel.
const COL_STATION: usize = 4; // D
const COL_RAILWAY: usize = 5; // E
const COL_ROAD_CAP: usize = 32; // AF — ёмкость путей (вагоны единовременно)
const COL_LOAD_CAP: usize = 41; // AO — погрузка в сутки

/// 1-based диапазон строк с данными (заголовок в 6–7).
const ROW_FIRST: usize = 8;
const ROW_LAST: usize = 1296;

#[derive(Debug, Default, Clone)]
struct Agg {
    rail_road: String,
    load_station: String,
    road_capacity: i64,
    load_capacity: i64,
}

/// Строка-запрос на поиск ЕСР (вход для Python-хелпера).
#[derive(Debug, Serialize)]
struct EsrQuery<'a> {
    station: &'a str,
    railway: &'a str,
}

/// Ответ Python-хелпера: тот же порядок, плюс найденный код (или null).
#[derive(Debug, Deserialize)]
struct EsrAnswer {
    #[serde(default)]
    code: Option<String>,
}

/// Итоговый объект справочника.
#[derive(Debug, Serialize)]
struct LoadStation {
    rail_road: String,
    load_station: String,
    load_station_code: Option<String>,
    station_road_capacity: i64,
    station_load_capacity: i64,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input = PathBuf::from("data/LoadStations.xlsx");
    let mut output = PathBuf::from("data/load_stations.json");
    let mut dry_run = false;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--input" | "-i" => {
                input = PathBuf::from(it.next().context("--input требует путь")?);
            }
            "--output" | "-o" => {
                output = PathBuf::from(it.next().context("--output требует путь")?);
            }
            "--dry-run" => dry_run = true,
            "-h" | "--help" => {
                println!(
                    "railoptim-load-stations [--input <xlsx>] [--output <json>] [--dry-run]"
                );
                return Ok(());
            }
            other => bail!("неизвестный аргумент: {other}"),
        }
    }

    let aggregated = parse_workbook(&input)?;
    eprintln!(
        "Excel разобран: {} уникальных станций (строки {ROW_FIRST}..={ROW_LAST})",
        aggregated.len()
    );

    let codes = if dry_run {
        eprintln!("--dry-run: пропускаю поиск ЕСР в MSSQL (code=null)");
        vec![None; aggregated.len()]
    } else {
        lookup_esr_codes(&aggregated)?
    };

    let mut matched = 0usize;
    let mut records: Vec<LoadStation> = Vec::with_capacity(aggregated.len());
    for (agg, code) in aggregated.into_iter().zip(codes.into_iter()) {
        if code.is_some() {
            matched += 1;
        }
        records.push(LoadStation {
            rail_road: agg.rail_road,
            load_station: agg.load_station,
            load_station_code: code,
            station_road_capacity: agg.road_capacity,
            station_load_capacity: agg.load_capacity,
        });
    }

    let json = serde_json::to_string_pretty(&records).context("сериализация JSON")?;
    std::fs::write(&output, json.as_bytes())
        .with_context(|| format!("запись {}", output.display()))?;

    eprintln!(
        "Готово: {} записей -> {} (с кодом ЕСР: {}, без кода: {})",
        records.len(),
        output.display(),
        matched,
        records.len() - matched
    );
    Ok(())
}

/// Парсит xlsx и агрегирует мощности по паре (станция, дорога).
fn parse_workbook(path: &Path) -> Result<Vec<Agg>> {
    let mut wb = open_workbook_auto(path)
        .with_context(|| format!("открытие {}", path.display()))?;
    let sheet = wb
        .sheet_names()
        .first()
        .cloned()
        .context("в книге нет листов")?;
    let range = wb
        .worksheet_range(&sheet)
        .with_context(|| format!("чтение листа {sheet}"))?;

    // Ключ агрегации — (станция, дорога) без учёта регистра/пробелов;
    // отображаемые значения берём из первой встреченной строки.
    let mut map: BTreeMap<(String, String), Agg> = BTreeMap::new();

    for row in ROW_FIRST..=ROW_LAST {
        let r = (row - 1) as u32; // calamine 0-based
        let station = cell_string(&range, r, COL_STATION - 1);
        if station.is_empty() {
            continue;
        }
        let railway = cell_string(&range, r, COL_RAILWAY - 1);
        let road_cap = cell_capacity(&range, r, COL_ROAD_CAP - 1);
        let load_cap = cell_capacity(&range, r, COL_LOAD_CAP - 1);

        let key = (station.to_lowercase(), railway.to_uppercase());
        let entry = map.entry(key).or_insert_with(|| Agg {
            rail_road: railway.clone(),
            load_station: station.clone(),
            road_capacity: 0,
            load_capacity: 0,
        });
        entry.road_capacity += road_cap.unwrap_or(0);
        entry.load_capacity += load_cap.unwrap_or(0);
    }

    Ok(map.into_values().collect())
}

/// Значение ячейки как строка (trim, NBSP→пробел, схлопывание пробелов).
fn cell_string(range: &calamine::Range<Data>, row: u32, col: usize) -> String {
    match range.get((row as usize, col)) {
        Some(Data::String(s)) => normalize_ws(s),
        Some(Data::Int(i)) => i.to_string(),
        Some(Data::Float(f)) => {
            if (f.fract()).abs() < f64::EPSILON {
                (*f as i64).to_string()
            } else {
                f.to_string()
            }
        }
        Some(d) if !d.is_empty() => normalize_ws(&d.to_string()),
        _ => String::new(),
    }
}

/// Парсит мощность из ячейки AF/AO.
///
/// Правила: «6-11» → среднее целое (округление к ближайшему, .5 вверх);
/// «31 и более» / «5 и менее» / «8» → одно число; текст без цифр → None.
fn cell_capacity(range: &calamine::Range<Data>, row: u32, col: usize) -> Option<i64> {
    match range.get((row as usize, col)) {
        Some(Data::Int(i)) => Some(*i),
        Some(Data::Float(f)) => Some(f.round() as i64),
        Some(Data::String(s)) => parse_capacity_str(s),
        Some(d) if !d.is_empty() => parse_capacity_str(&d.to_string()),
        _ => None,
    }
}

/// Парсер строковой мощности (см. [`cell_capacity`]).
fn parse_capacity_str(raw: &str) -> Option<i64> {
    let s = raw.replace('\u{00a0}', " ");
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Числовые токены по порядку.
    let nums: Vec<i64> = s
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse::<i64>().ok())
        .collect();
    if nums.is_empty() {
        return None;
    }

    let has_dash = s.contains('-') || s.contains('\u{2013}') || s.contains('\u{2014}');
    if has_dash && nums.len() >= 2 {
        // Диапазон «a-b» → среднее целое, округление .5 вверх.
        let avg = (nums[0] + nums[1]) as f64 / 2.0;
        return Some((avg + 0.5).floor() as i64);
    }

    Some(nums[0])
}

fn normalize_ws(s: &str) -> String {
    s.replace('\u{00a0}', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Запускает `src/data/load_stations_esr.py`, передаёт станции+дороги, читает коды ЕСР.
fn lookup_esr_codes(aggregated: &[Agg]) -> Result<Vec<Option<String>>> {
    let script = std::env::current_dir()
        .context("текущая директория")?
        .join("src/data/load_stations_esr.py");
    if !script.exists() {
        bail!("не найден хелпер {}", script.display());
    }

    let queries: Vec<EsrQuery> = aggregated
        .iter()
        .map(|a| EsrQuery {
            station: &a.load_station,
            railway: &a.rail_road,
        })
        .collect();
    let payload = serde_json::to_vec(&queries).context("сериализация запроса ЕСР")?;

    let mut child = Command::new("python3")
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("запуск python3 {}", script.display()))?;

    child
        .stdin
        .take()
        .context("stdin хелпера недоступен")?
        .write_all(&payload)
        .context("запись запроса в хелпер")?;

    let output = child.wait_with_output().context("ожидание хелпера ЕСР")?;
    if !output.status.success() {
        bail!("load_stations_esr.py завершился с кодом {:?}", output.status.code());
    }

    let answers: Vec<EsrAnswer> =
        serde_json::from_slice(&output.stdout).context("разбор ответа хелпера ЕСР")?;
    if answers.len() != aggregated.len() {
        bail!(
            "хелпер вернул {} ответов, ожидалось {}",
            answers.len(),
            aggregated.len()
        );
    }

    Ok(answers.into_iter().map(|a| a.code).collect())
}

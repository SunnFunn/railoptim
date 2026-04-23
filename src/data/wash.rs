//! Станции промывки: `wash.py json` (MSSQL) и узлы спроса под «грязные» вагоны.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::node::{DemandNode, DemandPurpose, SupplyNode};

use super::references::normalize_etsng_code;
use super::StationRef;

fn wash_script_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("текущая директория")?
        .join("src/data/wash.py"))
}

#[derive(Debug, Clone)]
pub struct WashStation {
    pub station_name: String,
    pub station_code: String,
    pub railway_short: String,
    pub railway_code: String,
    pub capacity_per_day: i32,
    pub railway_wash_division: Option<String>,
}

/// Запускает `wash.py json`, читает JSON со stdout.
pub fn fetch_wash_stations() -> Result<Vec<WashStation>> {
    let script = wash_script_path()?;
    let output = Command::new("python3")
        .arg(&script)
        .arg("json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("python3 {}", script.display()))?;

    if !output.status.success() {
        anyhow::bail!(
            "wash.py json: {:?}\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout).context("stdout wash.py UTF-8")?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() || trimmed == "[]" {
        return Ok(Vec::new());
    }

    let rows: Vec<serde_json::Value> =
        serde_json::from_str(trimmed).context("JSON станций промывки")?;

    let mut out = Vec::new();
    for r in rows {
        let obj = r.as_object().context("элемент wash JSON — не объект")?;
        let station_name = obj
            .get("StationWash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let station_code = obj
            .get("StationWashCode")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let railway_short = obj
            .get("RailWayWash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let railway_code = match obj.get("RailWayWashCode") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        let cap = obj
            .get("WashCapacity")
            .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
            .unwrap_or(0) as i32;
        let railway_wash_division = obj
            .get("RailWayWashDivision")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        if station_code.is_empty() || railway_short.is_empty() {
            continue;
        }
        out.push(WashStation {
            station_name,
            station_code,
            railway_short,
            railway_code,
            capacity_per_day: cap.max(0),
            railway_wash_division,
        });
    }
    Ok(out)
}

/// Верхняя граница суток планирования по периодам спроса (как в `demand::DEMAND_PERIODS`): 0..14 → 15 суток.
// const PLANNING_HORIZON_DAYS: i32 = 15;
pub const PLANNING_HORIZON_DAYS: i32 = 5;

/// Узлы спроса на промывку: «станция промывки» — пункт назначения грязных порожних.
///
/// `d_id` начинаются с `id_start` (обычно `load_demand.len() + 1`), чтобы не пересекаться с погрузкой в отчётах.
pub fn wash_demand_nodes(stations: &[WashStation], id_start: usize) -> Vec<DemandNode> {
    stations
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let cap = w
                .capacity_per_day
                .saturating_mul(PLANNING_HORIZON_DAYS)
                .max(w.capacity_per_day)
                .max(1);
            DemandNode {
                d_id: id_start + i,
                period: 1,
                purpose: DemandPurpose::Wash,
                station_name: w.station_name.clone(),
                station_code: w.station_code.clone(),
                railway_name: w.railway_short.clone(),
                railway_code: Some(w.railway_code.clone()),
                railway_part: w.railway_wash_division.clone(),
                station_to_name: None,
                station_to_code: None,
                railway_to_name: None,
                railway_to_code: None,
                railway_to_part: None,
                sender: None,
                sender_okpo: None,
                sender_tgnl: None,
                client: None,
                customer_okpo: None,
                recipient: None,
                loader_to_okpo: None,
                gng_cargo: Some("Промывка вагонов".to_string()),
                etsng: None,
                request_numbers: None,
                request_dates: None,
                gu12_number: None,
                shipping_type: None,
                car_type: None,
                car_count: cap,
                cars_on_station: 0,
            }
        })
        .collect()
}

/// Груженый вагон: по полю `GRPOName` / статусу.
pub fn supply_is_loaded(s: &SupplyNode) -> bool {
    s.status
        .as_deref()
        .map(|x| {
            let u = x.to_uppercase();
            u.contains("ГРУЖ") || u.contains("ГРЖ")
        })
        .unwrap_or(false)
}

/// Код ЕТСНГ для тарифа промывки: груженый — текущий (`FrETSNGCode` в АПИ), порожний — предыдущий груз.
pub fn effective_etsng_for_wash_tariff(s: &SupplyNode) -> Option<String> {
    if supply_is_loaded(s) {
        return s
            .etsng
            .as_deref()
            .map(normalize_etsng_code)
            .filter(|x| !x.is_empty());
    }
    dominant_nonempty_code(&s.prev_etsngs)
}

fn dominant_nonempty_code(codes: &[String]) -> Option<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for c in codes {
        let n = normalize_etsng_code(c);
        if n.is_empty() {
            continue;
        }
        *counts.entry(n).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(code, _)| code)
}

pub fn code_requires_wash(code: &str, wash_codes: &HashSet<String>) -> bool {
    let n = normalize_etsng_code(code);
    !n.is_empty() && wash_codes.contains(&n)
}

/// Узел предложения относится к грузам из списка промывки (по правилу груженый/порожний).
///
/// Не учитывает исключения по дороге образования — для полной проверки используй
/// [`supply_needs_wash`].
pub fn supply_matches_wash_product_list(s: &SupplyNode, wash_codes: &HashSet<String>) -> bool {
    effective_etsng_for_wash_tariff(s)
        .map(|c| code_requires_wash(&c, wash_codes))
        .unwrap_or(false)
}

/// Вагон является «грязным» и требует промывки с точки зрения российского планирования.
///
/// Возвращает `false` если выполнено **любое** из условий:
/// - груз вагона не входит в список `WashProductCodes`, или
/// - дорога образования вагона (`railway_to`) входит в `NoCleaningRoads`
///   (промывка на иностранной территории уже оплачена клиентом).
pub fn supply_needs_wash(
    s: &SupplyNode,
    wash_codes: &HashSet<String>,
    no_cleaning_roads: &HashSet<String>,
) -> bool {
    if no_cleaning_roads.contains(s.railway_to.trim()) {
        return false;
    }
    supply_matches_wash_product_list(s, wash_codes)
}

/// На станции текущего положения вагона (`station_to_code`) есть спрос на погрузку того же ЕТСНГ —
/// можно обойтись без промывки под тот же груз.
pub fn load_demand_covers_same_etsng(s: &SupplyNode, load_demands: &[DemandNode]) -> bool {
    let Some(eff) = effective_etsng_for_wash_tariff(s) else {
        return false;
    };
    load_demands.iter().any(|d| {
        d.purpose == DemandPurpose::Load
            && d
                .etsng
                .as_deref()
                .map(|e| normalize_etsng_code(e) == eff)
                .unwrap_or(false)
            && d.station_code.trim() == s.station_to_code.trim()
    })
}

/// Станции назначения — промывки (уникальные пары код+дорога).
pub fn wash_station_refs(stations: &[WashStation]) -> Vec<StationRef> {
    let mut set: HashSet<(String, String)> = HashSet::new();
    for w in stations {
        set.insert((w.station_code.clone(), w.railway_short.clone()));
    }
    let mut v: Vec<_> = set
        .into_iter()
        .map(|(code, rw)| StationRef::new(code, rw))
        .collect();
    v.sort_by(|a, b| {
        a.station_code
            .cmp(&b.station_code)
            .then_with(|| a.railway_short_name.cmp(&b.railway_short_name))
    });
    v
}

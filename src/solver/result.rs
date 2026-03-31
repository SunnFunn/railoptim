use std::fs;
use std::path::PathBuf;

use chrono::Local;
use serde::Serialize;

use crate::node::{DemandNode, SupplyNode, CarKind};
use super::lp::OptimResult;
use super::model::TaskArc;

// ---------------------------------------------------------------------------
// Структуры отчёта
// ---------------------------------------------------------------------------

/// Одна строка плана назначения: конкретный вагон (или группа) → узел спроса.
#[derive(Serialize, Debug)]
pub struct AssignmentRecord {
    /// Назначено вагонов.
    pub cars: f64,

    // --- Предложение ---
    pub supply_id:           usize,
    pub supply_kind:         &'static str,
    /// Номера вагонов в группе (пусто для NoNumber).
    pub car_numbers:         Vec<u64>,
    pub supply_station:      String,
    pub supply_station_code: String,
    pub supply_railway:      String,

    // --- Спрос ---
    pub demand_id:           usize,
    pub demand_station:      String,
    pub demand_station_code: String,
    pub demand_railway:      String,
    pub demand_period:       u8,

    // --- Тариф ---
    pub cost_rub:      f64,
    pub distance_km:   i32,
    pub delivery_days: i32,
    pub period_ok:     bool,
    pub car_type_ok:   bool,
}

/// Полный отчёт об одном прогоне оптимизации.
#[derive(Serialize, Debug)]
pub struct OptimReport {
    pub timestamp:       String,
    pub solver_status:   String,
    pub total_cost_rub:  f64,
    pub assigned_cars:   f64,
    pub penalty_cars:    f64,
    pub supply_count:    usize,
    pub demand_count:    usize,
    pub arc_count:       usize,
    pub assignments:     Vec<AssignmentRecord>,
}

// ---------------------------------------------------------------------------
// Построение отчёта
// ---------------------------------------------------------------------------

/// Строит полный отчёт из результата LP-решателя.
pub fn build_report(
    result:  &OptimResult,
    solution: &[f64],
    arcs:    &[TaskArc],
    supply:  &[SupplyNode],
    demand:  &[DemandNode],
) -> OptimReport {
    let assignments = arcs
        .iter()
        .zip(solution.iter())
        .filter(|(_, qty)| **qty > 1e-4)
        .map(|(arc, &cars)| {
            let s = &supply[arc.s_idx];
            let d = &demand[arc.d_idx];
            AssignmentRecord {
                cars,
                supply_id:           s.s_id,
                supply_kind:         car_kind_str(&s.kind),
                car_numbers:         s.car_numbers.clone(),
                supply_station:      s.station_to.clone(),
                supply_station_code: arc.supply_station_code.clone(),
                supply_railway:      s.railway_to.clone(),
                demand_id:           d.d_id,
                demand_station:      d.station_name.clone(),
                demand_station_code: arc.demand_station_code.clone(),
                demand_railway:      d.railway_name.clone(),
                demand_period:       d.period,
                cost_rub:            arc.cost,
                distance_km:         arc.distance,
                delivery_days:       arc.delivery_days,
                period_ok:           arc.period_ok,
                car_type_ok:         arc.car_type_ok,
            }
        })
        .collect();

    OptimReport {
        timestamp:      Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        solver_status:  result.status.clone(),
        total_cost_rub: result.total_cost,
        assigned_cars:  result.assigned_cars,
        penalty_cars:   result.penalty_cars,
        supply_count:   supply.len(),
        demand_count:   demand.len(),
        arc_count:      arcs.len(),
        assignments,
    }
}

// ---------------------------------------------------------------------------
// Сохранение на диск
// ---------------------------------------------------------------------------

/// Сохраняет отчёт в `tmp/result_YYYYMMDD_HHMMSS.json`.
///
/// Директория `tmp/` создаётся автоматически при отсутствии.
pub fn save_result(report: &OptimReport) -> anyhow::Result<PathBuf> {
    let dir = PathBuf::from("tmp");
    fs::create_dir_all(&dir)?;

    let filename = format!(
        "result_{}.json",
        Local::now().format("%Y%m%d_%H%M%S")
    );
    let path = dir.join(filename);

    let json = serde_json::to_string_pretty(report)?;
    fs::write(&path, json)?;

    Ok(path)
}

// ---------------------------------------------------------------------------
// Вспомогательное
// ---------------------------------------------------------------------------

fn car_kind_str(kind: &CarKind) -> &'static str {
    match kind {
        CarKind::Free     => "Free",
        CarKind::Assigned => "Assigned",
        CarKind::NoNumber => "NoNumber",
    }
}

pub fn period_range_str(period: u8) -> &'static str {
    match period {
        1 => "1-5",
        2 => "6-8",
        3 => "9-10",
        4 => "11-15",
        _ => "?",
    }
}

// ---------------------------------------------------------------------------
// Выходные данные для АПИ (схема request.json / DestinationRegistryTransmission)
// ---------------------------------------------------------------------------

/// Одна запись плана назначения в формате API.
///
/// Поля `supply_kind` и `period_label` помечены `#[serde(skip)]`
/// — они не отправляются в API, но используются для листа Excel.
#[derive(Serialize, Debug, Clone)]
pub struct OutputRecord {
    #[serde(rename = "OPZDate")]
    pub opz_date: String,

    // --- Откуда (узел предложения) ---
    #[serde(rename = "RailWayFrom")]
    pub railway_from: String,
    #[serde(rename = "RailWayFromDivision")]
    pub railway_from_div: Option<String>,
    #[serde(rename = "StationFrom")]
    pub station_from: String,
    #[serde(rename = "StationFromCode")]
    pub station_from_code: String,

    // --- Куда (узел спроса) ---
    #[serde(rename = "RailWayTo")]
    pub railway_to: String,
    #[serde(rename = "RailWayToDivision")]
    pub railway_to_div: Option<String>,
    #[serde(rename = "StationTo")]
    pub station_to: String,
    #[serde(rename = "StationToCode")]
    pub station_to_code: String,

    // --- Назначение ---
    #[serde(rename = "AssignedCarsAmount")]
    pub assigned_cars: i32,
    #[serde(rename = "LoadStatus")]
    pub load_status: Option<String>,
    #[serde(rename = "CarType")]
    pub car_type: Option<String>,

    // --- Груз ---
    #[serde(rename = "PrevFrETSNGName")]
    pub prev_etsng_name: Option<String>,
    #[serde(rename = "FrETSNGName")]
    pub etsng_name: Option<String>,

    // --- Заявка ---
    #[serde(rename = "GU12Number")]
    pub gu12_number: Option<String>,
    #[serde(rename = "ClaimNumber")]
    pub claim_number: Option<String>,
    #[serde(rename = "ClaimDate")]
    pub claim_date: Option<String>,

    // --- Участники ---
    #[serde(rename = "Client")]
    pub client: Option<String>,
    #[serde(rename = "Sender")]
    pub sender: Option<String>,
    #[serde(rename = "Customer")]
    pub customer: Option<String>,

    // --- Тариф ---
    #[serde(rename = "Distance")]
    pub distance: i32,
    #[serde(rename = "PeriodOfDelivery")]
    pub period_of_delivery: i32,
    #[serde(rename = "Cost")]
    pub cost: f64,

    // --- Тип назначения ---
    #[serde(rename = "AssignmentType")]
    pub assignment_type: String,

    // --- Номера вагонов ---
    #[serde(rename = "CarNumbersList")]
    pub car_numbers_list: Vec<String>,

    // --- Только для Excel (не отправляется в API) ---
    #[serde(skip)]
    pub supply_kind: String,
    #[serde(skip)]
    pub period_label: String,
}

/// Строит список записей для отправки в API из результата оптимизации.
pub fn build_output_records(
    solution: &[f64],
    arcs:     &[TaskArc],
    supply:   &[SupplyNode],
    demand:   &[DemandNode],
) -> Vec<OutputRecord> {
    let now_str = Local::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

    arcs.iter()
        .zip(solution.iter())
        .filter(|(_, qty)| **qty > 1e-4)
        .map(|(arc, &qty)| {
            let s = &supply[arc.s_idx];
            let d = &demand[arc.d_idx];

            let period_label = period_range_str(d.period).to_string();

            OutputRecord {
                opz_date:          now_str.clone(),
                railway_from:      s.railway_to.clone(),
                railway_from_div:  s.railway_part_to.clone(),
                station_from:      s.station_to.clone(),
                station_from_code: s.station_to_code.clone(),
                railway_to:        d.railway_name.clone(),
                railway_to_div:    d.railway_part.clone(),
                station_to:        d.station_name.clone(),
                station_to_code:   d.station_code.clone(),
                assigned_cars:     qty.round() as i32,
                load_status:       s.status.clone(),
                car_type:          s.car_type.clone(),
                prev_etsng_name:   s.prev_etsng_names.first().cloned(),
                etsng_name:        s.etsng_name.clone(),
                gu12_number:       d.gu12_number.as_ref().and_then(|v| v.first().cloned()),
                claim_number:      d.request_numbers.as_ref().and_then(|v| v.first().cloned()),
                claim_date:        d.request_dates.as_ref().and_then(|v| v.first().cloned()),
                client:            d.client.as_ref().and_then(|v| v.first().cloned()),
                sender:            d.sender.clone(),
                customer:          d.recipient.as_ref().and_then(|v| v.first().cloned()),
                distance:          arc.distance,
                period_of_delivery: arc.delivery_days,
                cost:              arc.cost,
                assignment_type:   format!("Под погрузку в {period_label} сутки"),
                car_numbers_list:  s.car_numbers.iter().map(|n| n.to_string()).collect(),
                supply_kind:       car_kind_str(&s.kind).to_string(),
                period_label,
            }
        })
        .collect()
}

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
    pub car_number:          Option<u64>,
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
    pub cost_rub:      i64,
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
                car_number:          s.car_number,
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

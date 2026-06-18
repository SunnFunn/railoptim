//! Этап 3: размещение оставшегося излишка порожних вагонов на свободных подъездных
//! путях крупных станций погрузки (справочник `data/load_stations_free_capacity.json`).
//!
//! Запускается **после** основного решения и этапа отстоя: на вход поступают только
//! вагоны, не назначенные ни заявкам, ни в отстой (`excess` — остаток после резервов).
//!
//! В отличие от отстоя здесь действует ограничение **минимальной партии на станцию**:
//! на одну станцию погрузки назначается либо 0, либо не менее [`LOADROAD_MIN_BATCH`]
//! вагонов. Поэтому это уже MIP (HiGHS), с бинарной переменной `y_l` на станцию и
//! big-M дизъюнкцией (как в основном решателе, см. `mip.rs`):
//!
//! ```text
//! B * y_l  ≤  Σ_s x[s,l]            (либо 0, либо ≥ B вагонов на станцию l)
//! Σ_s x[s,l]  ≤  cap_l * y_l        (поток только при y_l = 1; верхняя граница — ёмкость)
//! Σ_l x[s,l]  ≤  excess[s]          (по узлам излишка)
//! min Σ (cost[s,l] − PLACEMENT_REWARD) · x   (сначала максимум размещённых, затем тариф)
//! ```
//!
//! Ограничения типа вагона, промывки и ДМЗИ к этому этапу не применяются.

use std::collections::HashMap;

use highs::{ColProblem, Sense};

use crate::data::free_loadroads::FreeLoadRoad;
use crate::node::{SupplyNode, TariffNode};

/// «Премия» за размещение одного вагона на пути станции погрузки.
///
/// Заведомо выше максимального реального тарифа (~700 тыс. руб.), поэтому решатель
/// сначала максимизирует число размещённых вагонов и лишь затем минимизирует тариф
/// (аналог [`super::reserve::RESERVE_PLACEMENT_REWARD`]).
pub const LOADROAD_PLACEMENT_REWARD: f64 = 1_000_000.0;

/// Минимальная партия вагонов на одну станцию погрузки: либо 0, либо ≥ этого значения.
pub const LOADROAD_MIN_BATCH: i32 = 5;

/// Назначение группы вагонов излишка на пути станции погрузки.
#[derive(Debug, Clone)]
pub struct LoadRoadAssignment {
    /// Индекс узла предложения (в `opt_supply`).
    pub s_idx: usize,
    /// Индекс станции погрузки (в срезе `loadroads`).
    pub l_idx: usize,
    /// Вагонов направлено на пути станции.
    pub quantity: i32,
    /// Тариф за вагон, руб.
    pub cost: f64,
    /// Расстояние, км.
    pub distance: i32,
    /// Срок доставки, сутки.
    pub delivery_days: i32,
}

/// Решает задачу размещения излишка на путях станций погрузки (MIP, min-batch на станцию).
///
/// - `excess[s_idx]` — остаток вагонов узла предложения после основного решения и отстоя;
/// - `tariffs` — карта `(код станции предложения, код станции погрузки) → тариф`
///   (направление: **от** станции дислокации порожнего `SupplyNode::station_to_code`
///   **к** станции погрузки `FreeLoadRoad::load_station_code`);
/// - пары без тарифа переменной не получают;
/// - на каждую станцию погрузки назначается 0 или ≥ [`LOADROAD_MIN_BATCH`] вагонов.
pub fn solve_loadroad_assignment(
    excess: &[i32],
    supply: &[SupplyNode],
    loadroads: &[FreeLoadRoad],
    tariffs: &HashMap<(String, String), TariffNode>,
) -> Vec<LoadRoadAssignment> {
    let mut model = ColProblem::default();

    // Строки излишка: только узлы с положительным остатком.
    let mut supply_rows: HashMap<usize, highs::Row> = HashMap::new();
    for (s_idx, &rem) in excess.iter().enumerate() {
        if rem > 0 {
            supply_rows.insert(s_idx, model.add_row(0.0..=rem as f64));
        }
    }
    if supply_rows.is_empty() || loadroads.is_empty() {
        return Vec::new();
    }

    // На каждую станцию погрузки — пара строк big-M дизъюнкции (обе ≤ 0):
    //   lower: B·y − Σx ≤ 0,  upper: Σx − cap·y ≤ 0.
    let lower_rows: Vec<_> = loadroads
        .iter()
        .map(|_| model.add_row(f64::NEG_INFINITY..=0.0))
        .collect();
    let upper_rows: Vec<_> = loadroads
        .iter()
        .map(|_| model.add_row(f64::NEG_INFINITY..=0.0))
        .collect();

    // Переменные потока x[s,l] (целые) — только пары с известным тарифом.
    let mut cols: Vec<(usize, usize, f64, i32, i32)> = Vec::new();
    let mut sorted_s: Vec<usize> = supply_rows.keys().copied().collect();
    sorted_s.sort_unstable();
    for &s_idx in &sorted_s {
        let from_code = supply[s_idx].station_to_code.as_str();
        let rem = excess[s_idx];
        for (l_idx, l) in loadroads.iter().enumerate() {
            let cap = l.free_rail_road_capacity.max(0) as i32;
            if cap == 0 {
                continue;
            }
            let Some(t) = tariffs.get(&(from_code.to_string(), l.load_station_code.clone())) else {
                continue;
            };
            let upper = rem.min(cap).max(0) as f64;
            model.add_integer_column(
                t.cost - LOADROAD_PLACEMENT_REWARD,
                0.0..=upper,
                [
                    (supply_rows[&s_idx], 1.0),
                    (lower_rows[l_idx], -1.0),
                    (upper_rows[l_idx], 1.0),
                ],
            );
            cols.push((s_idx, l_idx, t.cost, t.distance, t.period_of_delivery));
        }
    }
    if cols.is_empty() {
        return Vec::new();
    }

    // Бинарные y_l ∈ {0,1}: B·y в lower-строке, −cap·y в upper-строке.
    // y добавляются ПОСЛЕ всех x — порядок столбцов: [x..., y...].
    for (l_idx, l) in loadroads.iter().enumerate() {
        let cap = l.free_rail_road_capacity.max(0) as i32;
        let b = LOADROAD_MIN_BATCH.min(cap).max(0) as f64;
        model.add_integer_column(
            0.0,
            0.0..=1.0,
            [
                (lower_rows[l_idx], b),
                (upper_rows[l_idx], -(cap as f64)),
            ],
        );
    }

    let mut optimizer = model.optimise(Sense::Minimise);
    optimizer.set_option("presolve", "on");
    optimizer.set_option("parallel", "on");
    let solved = optimizer.solve();
    let col_vals = solved.get_solution().columns().to_vec();

    // Первые cols.len() столбцов — переменные x.
    cols.iter()
        .zip(col_vals.iter())
        .filter(|&(_, &v)| v > 0.5)
        .map(|(&(s_idx, l_idx, cost, distance, delivery_days), &v)| LoadRoadAssignment {
            s_idx,
            l_idx,
            quantity: v.round() as i32,
            cost,
            distance,
            delivery_days,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{CarKind, RepairStatus};

    fn supply_at(code: &str, count: i32) -> SupplyNode {
        SupplyNode {
            s_id: 1,
            kind: CarKind::Free,
            car_count: count,
            station_to: format!("Ст-{code}"),
            station_to_code: code.to_string(),
            railway_to: "МСК".to_string(),
            railway_to_code: None,
            railway_part_to: None,
            car_type: None,
            etsng: None,
            etsng_name: None,
            repair_status: RepairStatus::Ok,
            status: None,
            supply_period: 1,
            car_numbers: vec![],
            stations_from: vec![],
            stations_from_code: vec![],
            railways_from: vec![],
            railways_from_code: vec![],
            railways_part_from: vec![],
            is_mass_unloading: false,
            prev_etsngs: vec![],
            prev_etsng_names: vec![],
        }
    }

    fn loadroad(code: &str, free: i64) -> FreeLoadRoad {
        FreeLoadRoad {
            load_road_name: "ПРВ".to_string(),
            load_station_name: format!("Погр-{code}"),
            load_station_code: code.to_string(),
            rail_road_capacity: free + 10,
            cars_on_rail_roads: 10,
            free_rail_road_capacity: free,
        }
    }

    fn tariff(from: &str, to: &str, cost: f64) -> ((String, String), TariffNode) {
        (
            (from.to_string(), to.to_string()),
            TariffNode {
                station_from: from.to_string(),
                station_from_code: from.to_string(),
                railway_from: "МСК".to_string(),
                railway_from_code: 17,
                station_to: to.to_string(),
                station_to_code: to.to_string(),
                railway_to: "ПРВ".to_string(),
                railway_to_code: 61,
                distance: 100,
                period_of_delivery: 2,
                cost,
                actual_date: chrono::NaiveDate::from_ymd_opt(2026, 6, 11)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            },
        )
    }

    /// Ёмкость станции не превышается: из 10 вагонов размещаются только 6.
    #[test]
    fn capacity_is_respected() {
        let supply = vec![supply_at("S1", 10)];
        let roads = vec![loadroad("L1", 6)];
        let tariffs: HashMap<_, _> = [tariff("S1", "L1", 10_000.0)].into();
        let a = solve_loadroad_assignment(&[10], &supply, &roads, &tariffs);
        assert_eq!(a.iter().map(|x| x.quantity).sum::<i32>(), 6);
    }

    /// При достаточной ёмкости выбирается более дешёвая станция.
    #[test]
    fn cheaper_station_preferred() {
        let supply = vec![supply_at("S1", 8)];
        let roads = vec![loadroad("L1", 100), loadroad("L2", 100)];
        let tariffs: HashMap<_, _> =
            [tariff("S1", "L1", 50_000.0), tariff("S1", "L2", 10_000.0)].into();
        let a = solve_loadroad_assignment(&[8], &supply, &roads, &tariffs);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].l_idx, 1);
        assert_eq!(a[0].quantity, 8);
    }

    /// Min-batch на станцию: меньше 5 вагонов поставить нельзя.
    /// Доступно только 3 вагона на единственную станцию → размещение 0.
    #[test]
    fn min_batch_blocks_small_placement() {
        let supply = vec![supply_at("S1", 3)];
        let roads = vec![loadroad("L1", 100)];
        let tariffs: HashMap<_, _> = [tariff("S1", "L1", 10_000.0)].into();
        let a = solve_loadroad_assignment(&[3], &supply, &roads, &tariffs);
        assert!(a.is_empty(), "3 < MIN_BATCH(5) — размещения быть не должно");
    }

    /// Ровно min-batch (5) — размещение допустимо.
    #[test]
    fn exactly_min_batch_allowed() {
        let supply = vec![supply_at("S1", 5)];
        let roads = vec![loadroad("L1", 100)];
        let tariffs: HashMap<_, _> = [tariff("S1", "L1", 10_000.0)].into();
        let a = solve_loadroad_assignment(&[5], &supply, &roads, &tariffs);
        assert_eq!(a.iter().map(|x| x.quantity).sum::<i32>(), 5);
    }

    /// Несколько узлов предложения могут набрать партию на одну станцию совместно.
    #[test]
    fn multiple_supplies_share_one_station() {
        let supply = vec![supply_at("S1", 3), supply_at("S2", 4)];
        let roads = vec![loadroad("L1", 100)];
        let tariffs: HashMap<_, _> =
            [tariff("S1", "L1", 10_000.0), tariff("S2", "L1", 10_000.0)].into();
        let a = solve_loadroad_assignment(&[3, 4], &supply, &roads, &tariffs);
        // 3 + 4 = 7 ≥ 5 — станция используется, размещены все 7.
        assert_eq!(a.iter().map(|x| x.quantity).sum::<i32>(), 7);
    }

    /// Пара без тарифа переменной не получает.
    #[test]
    fn no_tariff_no_assignment() {
        let supply = vec![supply_at("S1", 10)];
        let roads = vec![loadroad("L1", 100)];
        let tariffs: HashMap<_, _> = HashMap::new();
        let a = solve_loadroad_assignment(&[10], &supply, &roads, &tariffs);
        assert!(a.is_empty());
    }

    /// Нулевой излишек — пустой результат без запуска решателя.
    #[test]
    fn empty_excess_returns_empty() {
        let supply = vec![supply_at("S1", 0)];
        let roads = vec![loadroad("L1", 100)];
        let tariffs: HashMap<_, _> = [tariff("S1", "L1", 10_000.0)].into();
        let a = solve_loadroad_assignment(&[0], &supply, &roads, &tariffs);
        assert!(a.is_empty());
    }
}

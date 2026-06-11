//! Этап 2: размещение излишка порожних вагонов в узлы отстоя (резервы).
//!
//! Запускается **после** основного решения (greedy → MIP → ALNS): на вход
//! поступают только вагоны, которые основной задаче не удалось назначить
//! (`remaining_supply_vec`), поэтому отстой структурно не конкурирует
//! с заявками клиентов.
//!
//! Малая транспортная задача (HiGHS LP; матрица ограничений тотально
//! унимодулярна, целочисленные границы — решение целочисленно):
//! - `Σ x[s→r] ≤ excess[s]` — по узлам излишка;
//! - `Σ x[s→r] ≤ capacity[r]` — по узлам отстоя;
//! - `min Σ (cost[s→r] − PLACEMENT_REWARD)·x` — сначала максимум размещённых
//!   вагонов, затем минимум тарифной стоимости.
//!
//! Ограничения типа вагона, промывки, MIN_BATCH и ДМЗИ к отстою не применяются.

use std::collections::HashMap;

use highs::{ColProblem, Sense};

use crate::node::{ReserveNode, SupplyNode, TariffNode};

/// «Премия» за размещение одного вагона в отстой.
///
/// Заведомо выше максимального реального тарифа (~700 тыс. руб.), поэтому
/// решатель сначала максимизирует число размещённых вагонов и лишь затем
/// минимизирует стоимость (аналог `PENALTY_UNMET` в основной задаче).
pub const RESERVE_PLACEMENT_REWARD: f64 = 1_000_000.0;

/// Назначение группы вагонов излишка в узел отстоя.
#[derive(Debug, Clone)]
pub struct ReserveAssignment {
    /// Индекс узла предложения (в `opt_supply`).
    pub s_idx: usize,
    /// Индекс узла отстоя (в массиве резервов).
    pub r_idx: usize,
    /// Вагонов направлено в отстой.
    pub quantity: i32,
    /// Тариф за вагон, руб.
    pub cost: f64,
    /// Расстояние, км.
    pub distance: i32,
    /// Срок доставки, сутки.
    pub delivery_days: i32,
}

/// Решает задачу размещения излишка в резервы.
///
/// - `excess[s_idx]` — остаток вагонов узла предложения после основного решения;
/// - `tariffs` — карта `(код станции предложения, код станции отстоя) → тариф`
///   (направление: **от** станции дислокации порожнего `SupplyNode::station_to_code`
///   **к** станции резерва);
/// - пары без тарифа переменной не получают.
pub fn solve_reserve_assignment(
    excess: &[i32],
    supply: &[SupplyNode],
    reserves: &[ReserveNode],
    tariffs: &HashMap<(String, String), TariffNode>,
) -> Vec<ReserveAssignment> {
    let mut model = ColProblem::default();

    // Строки излишка: только узлы с положительным остатком.
    let mut supply_rows: HashMap<usize, highs::Row> = HashMap::new();
    for (s_idx, &rem) in excess.iter().enumerate() {
        if rem > 0 {
            supply_rows.insert(s_idx, model.add_row(0.0..=rem as f64));
        }
    }
    if supply_rows.is_empty() || reserves.is_empty() {
        return Vec::new();
    }

    let reserve_rows: Vec<_> = reserves
        .iter()
        .map(|r| model.add_row(0.0..=r.capacity.max(0) as f64))
        .collect();

    // Переменные: (s, r) с известным тарифом.
    let mut cols: Vec<(usize, usize, f64, i32, i32)> = Vec::new();
    let mut sorted_s: Vec<usize> = supply_rows.keys().copied().collect();
    sorted_s.sort_unstable();
    for &s_idx in &sorted_s {
        let from_code = supply[s_idx].station_to_code.as_str();
        for (r_idx, r) in reserves.iter().enumerate() {
            let Some(t) = tariffs.get(&(from_code.to_string(), r.station_code.clone())) else {
                continue;
            };
            model.add_column(
                t.cost - RESERVE_PLACEMENT_REWARD,
                0.0..,
                [
                    (supply_rows[&s_idx], 1.0),
                    (reserve_rows[r_idx], 1.0),
                ],
            );
            cols.push((s_idx, r_idx, t.cost, t.distance, t.period_of_delivery));
        }
    }
    if cols.is_empty() {
        return Vec::new();
    }

    let mut optimizer = model.optimise(Sense::Minimise);
    optimizer.set_option("solver", "simplex");
    optimizer.set_option("presolve", "on");
    let solved = optimizer.solve();
    let col_vals = solved.get_solution().columns().to_vec();

    cols.iter()
        .zip(col_vals.iter())
        .filter(|&(_, &v)| v > 0.5)
        .map(|(&(s_idx, r_idx, cost, distance, delivery_days), &v)| ReserveAssignment {
            s_idx,
            r_idx,
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

    fn reserve_at(code: &str, capacity: i32) -> ReserveNode {
        ReserveNode {
            r_id: 1,
            station_name: format!("Отстой-{code}"),
            station_code: code.to_string(),
            railway_short: "МСК".to_string(),
            railway_code: None,
            division: None,
            owner: Some("ООО Отстой".to_string()),
            owner_okpo: None,
            agreement_number: None,
            capacity,
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
                railway_to: "МСК".to_string(),
                railway_to_code: 17,
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

    /// Ёмкость резерва не превышается: из 5 вагонов размещаются только 3.
    #[test]
    fn capacity_is_respected() {
        let supply = vec![supply_at("S1", 5)];
        let reserves = vec![reserve_at("R1", 3)];
        let tariffs: HashMap<_, _> = [tariff("S1", "R1", 10_000.0)].into();
        let a = solve_reserve_assignment(&[5], &supply, &reserves, &tariffs);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].quantity, 3);
    }

    /// При достаточной ёмкости выбирается более дешёвый резерв.
    #[test]
    fn cheaper_reserve_preferred() {
        let supply = vec![supply_at("S1", 4)];
        let reserves = vec![reserve_at("R1", 10), reserve_at("R2", 10)];
        let tariffs: HashMap<_, _> =
            [tariff("S1", "R1", 50_000.0), tariff("S1", "R2", 10_000.0)].into();
        let a = solve_reserve_assignment(&[4], &supply, &reserves, &tariffs);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].r_idx, 1);
        assert_eq!(a[0].quantity, 4);
    }

    /// Размещение максимизируется даже в дорогой резерв (премия выше тарифа).
    #[test]
    fn placement_maximized_over_cost() {
        let supply = vec![supply_at("S1", 2)];
        let reserves = vec![reserve_at("R1", 1), reserve_at("R2", 1)];
        let tariffs: HashMap<_, _> =
            [tariff("S1", "R1", 5_000.0), tariff("S1", "R2", 900_000.0)].into();
        let a = solve_reserve_assignment(&[2], &supply, &reserves, &tariffs);
        let placed: i32 = a.iter().map(|x| x.quantity).sum();
        assert_eq!(placed, 2);
    }

    /// Пара без тарифа переменной не получает: вагоны остаются неразмещёнными.
    #[test]
    fn no_tariff_no_assignment() {
        let supply = vec![supply_at("S1", 3), supply_at("S2", 2)];
        let reserves = vec![reserve_at("R1", 10)];
        let tariffs: HashMap<_, _> = [tariff("S2", "R1", 10_000.0)].into();
        let a = solve_reserve_assignment(&[3, 2], &supply, &reserves, &tariffs);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].s_idx, 1);
        assert_eq!(a[0].quantity, 2);
    }

    /// Нулевой излишек — пустой результат без запуска решателя.
    #[test]
    fn empty_excess_returns_empty() {
        let supply = vec![supply_at("S1", 3)];
        let reserves = vec![reserve_at("R1", 10)];
        let tariffs: HashMap<_, _> = [tariff("S1", "R1", 10_000.0)].into();
        let a = solve_reserve_assignment(&[0], &supply, &reserves, &tariffs);
        assert!(a.is_empty());
    }
}

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
//! Конвенции РЖД (правило 5) закрывают отдельные пары станция→отстой.
//! Потолок дальности ([`crate::data::business_rules::BusinessRules::max_reserve_empty_run_km`]) отсекает пары
//! дальше 3000 км (ДВС/ЗАБ — 4100 км); тот же порог — у этапа 3 (пути клиента).

use std::collections::HashMap;

use highs::{ColProblem, Sense};

use crate::data::business_rules::BusinessRules;
use crate::data::convention_index::{ConventionIndex, ConventionScope, EmptyDestRef};
use crate::node::{ReserveNode, SupplyNode, TariffNode};
use super::model::supply_release_shift_days;

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
/// - пары без тарифа переменной не получают;
/// - `conventions` — запрет порожнего на станцию отстоя ([`ConventionIndex::disabled`] — без правила 5);
/// - `rules` — потолок дальности подсыла в отстой ([`BusinessRules::reserve_empty_run_too_far`];
///   [`BusinessRules::default()`] — без потолка).
pub fn solve_reserve_assignment(
    excess: &[i32],
    supply: &[SupplyNode],
    reserves: &[ReserveNode],
    tariffs: &HashMap<(String, String), TariffNode>,
    conventions: &ConventionIndex,
    rules: &BusinessRules,
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
    let mut convention_skip = 0usize;
    let mut distance_skip = 0usize;
    for &s_idx in &sorted_s {
        let s = &supply[s_idx];
        let from_code = s.station_to_code.as_str();
        let shift = supply_release_shift_days(s.supply_period);
        for (r_idx, r) in reserves.iter().enumerate() {
            let Some(t) = tariffs.get(&(from_code.to_string(), r.station_code.clone())) else {
                continue;
            };
            if rules.reserve_empty_run_too_far(&s.railway_to, t.distance) {
                distance_skip += 1;
                continue;
            }
            let rec_okpo: &[String] = match &r.owner_okpo {
                Some(o) => std::slice::from_ref(o),
                None => &[],
            };
            let rec_names: &[String] = match &r.owner {
                Some(n) => std::slice::from_ref(n),
                None => &[],
            };
            let dest = EmptyDestRef {
                supply_railway: &s.railway_to,
                supply_station_code: from_code,
                station_code: &r.station_code,
                station_name: &r.station_name,
                railway: &r.railway_short,
                sender_okpo: None,
                sender_name: None,
                recipient_okpos: rec_okpo,
                recipient_names: rec_names,
            };
            // Порожний в отстой: окно «отправление…прибытие».
            if conventions
                .ban_for_empty_dest_timed(dest, ConventionScope::Reserve, shift, shift + t.period_of_delivery)
                .is_some()
            {
                convention_skip += 1;
                continue;
            }
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
    if convention_skip > 0 {
        println!("  отстой: {convention_skip} пар станция→резерв закрыты конвенцией РЖД");
    }
    if distance_skip > 0 {
        println!("  отстой: {distance_skip} пар станция→резерв дальше потолка дальности");
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
    use crate::data::convention_index::ConventionIndex;
    use crate::data::conventions::{ConventionCargoClass, ConventionStatus, ParsedConvention};
    use crate::node::{CarKind, RepairStatus};

    fn solve(
        excess: &[i32],
        supply: &[SupplyNode],
        reserves: &[ReserveNode],
        tariffs: &HashMap<(String, String), TariffNode>,
    ) -> Vec<ReserveAssignment> {
        solve_reserve_assignment(
            excess,
            supply,
            reserves,
            tariffs,
            &ConventionIndex::disabled(),
            &BusinessRules::default(),
        )
    }

    fn solve_with_rules(
        excess: &[i32],
        supply: &[SupplyNode],
        reserves: &[ReserveNode],
        tariffs: &HashMap<(String, String), TariffNode>,
        rules: &BusinessRules,
    ) -> Vec<ReserveAssignment> {
        solve_reserve_assignment(
            excess,
            supply,
            reserves,
            tariffs,
            &ConventionIndex::disabled(),
            rules,
        )
    }

    fn reserve_cap_rules() -> BusinessRules {
        BusinessRules {
            max_reserve_empty_run_distance_km: Some(3000),
            max_reserve_empty_run_distance_far_east_km: Some(4100),
            reserve_far_east_railways: ["ДВС".into(), "ЗАБ".into()].into_iter().collect(),
            ..Default::default()
        }
    }

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
        tariff_dist(from, to, cost, 100)
    }

    fn tariff_dist(from: &str, to: &str, cost: f64, distance: i32) -> ((String, String), TariffNode) {
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
                distance,
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
        let a = solve(&[5], &supply, &reserves, &tariffs);
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
        let a = solve(&[4], &supply, &reserves, &tariffs);
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
        let a = solve(&[2], &supply, &reserves, &tariffs);
        let placed: i32 = a.iter().map(|x| x.quantity).sum();
        assert_eq!(placed, 2);
    }

    /// Пара без тарифа переменной не получает: вагоны остаются неразмещёнными.
    #[test]
    fn no_tariff_no_assignment() {
        let supply = vec![supply_at("S1", 3), supply_at("S2", 2)];
        let reserves = vec![reserve_at("R1", 10)];
        let tariffs: HashMap<_, _> = [tariff("S2", "R1", 10_000.0)].into();
        let a = solve(&[3, 2], &supply, &reserves, &tariffs);
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
        let a = solve(&[0], &supply, &reserves, &tariffs);
        assert!(a.is_empty());
    }

    fn conv_empty_esr(rzd: &str, esr: &str) -> ParsedConvention {
        ParsedConvention {
            rzd_number: rzd.into(),
            cargo_class: ConventionCargoClass::Empty,
            cargo_name: String::new(),
            date_beg: "2026-01-01".into(),
            date_end: "3000-01-01".into(),
            dest_esr: vec![esr.into()],
            dest_names: vec![],
            dest_railways: vec![],
            dest_all_stations: false,
            dep_esr: vec![],
            dep_names: vec![],
            dep_railways: vec![],
            dep_all_stations: false,
            junction: None,
            all_parties: true,
            recipient_okpo: vec![],
            recipient_names: vec![],
            unknown_road_fragments: vec![],
            convention_info: ConventionStatus::Other,
            kzh_stripped_dest: false,
            kzh_stripped_dep: false,
        }
    }

    /// Дешёвый отстой закрыт конвенцией — берём следующий.
    #[test]
    fn convention_skips_banned_reserve() {
        let supply = vec![supply_at("S1", 4)];
        let reserves = vec![reserve_at("987303", 10), reserve_at("R2", 10)];
        let tariffs: HashMap<_, _> =
            [tariff("S1", "987303", 10_000.0), tariff("S1", "R2", 50_000.0)].into();
        let idx = ConventionIndex::build(vec![conv_empty_esr("9001", "987303")]);
        let a = solve_reserve_assignment(
            &[4],
            &supply,
            &reserves,
            &tariffs,
            &idx,
            &BusinessRules::default(),
        );
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].r_idx, 1);
        assert_eq!(a[0].quantity, 4);
    }

    fn supply_at_rw(code: &str, count: i32, railway: &str) -> SupplyNode {
        let mut s = supply_at(code, count);
        s.railway_to = railway.to_string();
        s
    }

    /// С МСК дешёвый отстой на 8000 км отсекается — берём ближний дороже.
    #[test]
    fn distance_cap_skips_far_reserve_on_other_roads() {
        let supply = vec![supply_at_rw("S1", 4, "МСК")];
        let reserves = vec![reserve_at("R1", 10), reserve_at("R2", 10)];
        let tariffs: HashMap<_, _> = [
            tariff_dist("S1", "R1", 5_000.0, 8_000),
            tariff_dist("S1", "R2", 50_000.0, 500),
        ]
        .into();
        let a = solve_with_rules(&[4], &supply, &reserves, &tariffs, &reserve_cap_rules());
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].r_idx, 1);
        assert_eq!(a[0].quantity, 4);
        assert_eq!(a[0].distance, 500);
    }

    /// Ровно 3000 км с обычной дороги — ещё допустимо; 3001 — нет.
    #[test]
    fn distance_cap_3000_inclusive_for_other_roads() {
        let supply = vec![supply_at_rw("S1", 2, "СКВ")];
        let reserves = vec![reserve_at("R1", 10), reserve_at("R2", 10)];
        let tariffs: HashMap<_, _> = [
            tariff_dist("S1", "R1", 10_000.0, 3000),
            tariff_dist("S1", "R2", 10_000.0, 3001),
        ]
        .into();
        let a = solve_with_rules(&[2], &supply, &reserves, &tariffs, &reserve_cap_rules());
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].r_idx, 0);
        assert_eq!(a[0].distance, 3000);
    }

    /// С ДВС и ЗАБ порог 4100 км: 4100 проходит, 4101 нет.
    #[test]
    fn distance_cap_4100_for_dvs_and_zab() {
        let rules = reserve_cap_rules();
        for rw in ["ДВС", "ЗАБ"] {
            let supply = vec![supply_at_rw("S1", 2, rw)];
            let reserves = vec![reserve_at("R1", 10), reserve_at("R2", 10)];
            let tariffs: HashMap<_, _> = [
                tariff_dist("S1", "R1", 10_000.0, 4100),
                tariff_dist("S1", "R2", 1_000.0, 4101),
            ]
            .into();
            let a = solve_with_rules(&[2], &supply, &reserves, &tariffs, &rules);
            assert_eq!(a.len(), 1, "{rw}");
            assert_eq!(a[0].r_idx, 0, "{rw}");
            assert_eq!(a[0].distance, 4100, "{rw}");
        }
    }

    /// Без правил потолка 8000 км по-прежнему размещается (премия важнее тарифа).
    #[test]
    fn default_rules_allow_long_reserve_haul() {
        let supply = vec![supply_at_rw("S1", 2, "МСК")];
        let reserves = vec![reserve_at("R1", 10)];
        let tariffs: HashMap<_, _> = [tariff_dist("S1", "R1", 5_000.0, 8_000)].into();
        let a = solve(&[2], &supply, &reserves, &tariffs);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].quantity, 2);
    }
}

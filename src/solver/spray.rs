//! Назначение излишка на станции распыления до отстоя.
//!
//! Спрос узла — [`crate::data::spray::SprayCluster::assignment_capacity`]
//! (перспективная погрузка кластера, но не больше вместимости станции).
//! На одну станцию распыления назначается либо 0, либо не меньше
//! [`SPRAY_MIN_BATCH`] вагонов (MIP, как этап путей клиента). Предложение —
//! остаток после основной задачи. Сначала максимум размещённых вагонов,
//! затем минимум тарифа до станции распыления.

use std::collections::HashMap;

use highs::{ColProblem, Sense};

use crate::data::business_rules::BusinessRules;
use crate::data::convention_index::{ConventionIndex, ConventionScope, EmptyDestRef};
use crate::data::spray::SprayCluster;
use crate::node::{SupplyNode, TariffNode};
use super::model::supply_release_shift_days;
use super::reserve::RESERVE_PLACEMENT_REWARD;

/// Минимальная партия на одну станцию распыления: либо 0, либо не меньше этого числа.
pub const SPRAY_MIN_BATCH: i32 = 5;

/// Назначение группы вагонов излишка на станцию распыления.
#[derive(Debug, Clone)]
pub struct SprayAssignment {
    pub s_idx: usize,
    pub c_idx: usize,
    pub quantity: i32,
    pub cost: f64,
    pub distance: i32,
    pub delivery_days: i32,
}

/// Размещает излишек по кластерам распыления.
///
/// Пары без тарифа, дальше потолка дальности отстоя и закрытые конвенцией не строятся.
/// Кластер с ёмкостью меньше [`SPRAY_MIN_BATCH`] пропускается.
pub fn solve_spray_assignment(
    excess: &[i32],
    supply: &[SupplyNode],
    clusters: &[SprayCluster],
    tariffs: &HashMap<(String, String), TariffNode>,
    conventions: &ConventionIndex,
    rules: &BusinessRules,
) -> Vec<SprayAssignment> {
    let mut model = ColProblem::default();
    let mut supply_rows: HashMap<usize, highs::Row> = HashMap::new();
    for (s_idx, &rem) in excess.iter().enumerate() {
        if rem > 0 {
            supply_rows.insert(s_idx, model.add_row(0.0..=rem as f64));
        }
    }
    if supply_rows.is_empty() {
        return Vec::new();
    }

    // На каждую станцию с ёмкостью ≥ минимума — пара строк big-M (обе ≤ 0):
    //   lower: B·y − Σx ≤ 0,  upper: Σx − cap·y ≤ 0.
    let caps: Vec<i32> = clusters.iter().map(|c| c.assignment_capacity()).collect();
    let mut lower_rows: Vec<Option<highs::Row>> = Vec::with_capacity(clusters.len());
    let mut upper_rows: Vec<Option<highs::Row>> = Vec::with_capacity(clusters.len());
    for &cap in &caps {
        if cap >= SPRAY_MIN_BATCH {
            lower_rows.push(Some(model.add_row(f64::NEG_INFINITY..=0.0)));
            upper_rows.push(Some(model.add_row(f64::NEG_INFINITY..=0.0)));
        } else {
            lower_rows.push(None);
            upper_rows.push(None);
        }
    }

    let mut cols: Vec<(usize, usize, f64, i32, i32)> = Vec::new();
    let mut sorted_s: Vec<usize> = supply_rows.keys().copied().collect();
    sorted_s.sort_unstable();
    let mut convention_skip = 0usize;
    let mut distance_skip = 0usize;
    for &s_idx in &sorted_s {
        let s = &supply[s_idx];
        let from_code = s.station_to_code.as_str();
        let shift = supply_release_shift_days(s.supply_period);
        for (c_idx, cluster) in clusters.iter().enumerate() {
            let (Some(lower), Some(upper)) = (lower_rows[c_idx], upper_rows[c_idx]) else {
                continue;
            };
            let spray = &cluster.spray;
            let Some(t) = tariffs.get(&(from_code.to_string(), spray.station_code.clone())) else {
                continue;
            };
            if rules.reserve_empty_run_too_far(&s.railway_to, t.distance) {
                distance_skip += 1;
                continue;
            }
            let dest = EmptyDestRef {
                supply_railway: &s.railway_to,
                supply_station_code: from_code,
                station_code: &spray.station_code,
                station_name: &spray.station_name,
                railway: &spray.railway,
                sender_okpo: None,
                sender_name: None,
                recipient_okpos: &[],
                recipient_names: &[],
            };
            if conventions
                .ban_for_empty_dest_timed(
                    dest,
                    ConventionScope::Reserve,
                    shift,
                    shift + t.period_of_delivery,
                )
                .is_some()
            {
                convention_skip += 1;
                continue;
            }
            let col_upper = (excess[s_idx].min(caps[c_idx]).max(0)) as f64;
            model.add_integer_column(
                t.cost - RESERVE_PLACEMENT_REWARD,
                0.0..=col_upper,
                [
                    (supply_rows[&s_idx], 1.0),
                    (lower, -1.0),
                    (upper, 1.0),
                ],
            );
            cols.push((s_idx, c_idx, t.cost, t.distance, t.period_of_delivery));
        }
    }
    if convention_skip > 0 {
        println!("  распыление: {convention_skip} пар закрыты конвенцией РЖД");
    }
    if distance_skip > 0 {
        println!("  распыление: {distance_skip} пар дальше потолка дальности отстоя");
    }
    if cols.is_empty() {
        return Vec::new();
    }

    for (c_idx, &cap) in caps.iter().enumerate() {
        if cap < SPRAY_MIN_BATCH {
            continue;
        }
        let (Some(lower), Some(upper)) = (lower_rows[c_idx], upper_rows[c_idx]) else {
            continue;
        };
        model.add_integer_column(
            0.0,
            0.0..=1.0,
            [(lower, SPRAY_MIN_BATCH as f64), (upper, -(cap as f64))],
        );
    }

    let mut optimizer = model.optimise(Sense::Minimise);
    optimizer.set_option("presolve", "on");
    optimizer.set_option("parallel", "on");
    let solved = optimizer.solve();
    let col_vals = solved.get_solution().columns().to_vec();

    cols.iter()
        .zip(col_vals.iter())
        .filter(|&(_, &v)| v > 0.5)
        .map(|(&(s_idx, c_idx, cost, distance, delivery_days), &v)| SprayAssignment {
            s_idx,
            c_idx,
            quantity: v.round() as i32,
            cost,
            distance,
            delivery_days,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::spray::{SprayCluster, SprayStation};
    use crate::node::TariffNode;
    fn supply(code: &str, cars: i32) -> SupplyNode {
        SupplyNode {
            s_id: 1,
            kind: crate::node::CarKind::Free,
            car_count: cars,
            station_to: code.into(),
            station_to_code: code.into(),
            railway_to: "МСК".into(),
            railway_to_code: None,
            railway_part_to: None,
            car_type: None,
            etsng: None,
            etsng_name: None,
            repair_status: crate::node::RepairStatus::Ok,
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

    fn cluster(code: &str, load: i32, capacity: i32) -> SprayCluster {
        SprayCluster {
            spray: SprayStation {
                railway: "ЮВС".into(),
                station_name: code.into(),
                station_code: code.into(),
                station_capacity: capacity,
            },
            load_cars: load,
            members: vec![],
        }
    }

    fn tariff(from: &str, to: &str, km: i32, cost: f64) -> ((String, String), TariffNode) {
        (
            (from.into(), to.into()),
            TariffNode {
                station_from: String::new(),
                station_from_code: from.into(),
                railway_from: String::new(),
                railway_from_code: 0,
                station_to: String::new(),
                station_to_code: to.into(),
                railway_to: String::new(),
                railway_to_code: 0,
                distance: km,
                period_of_delivery: 2,
                cost,
                actual_date: chrono::NaiveDate::from_ymd_opt(2026, 6, 11)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            },
        )
    }

    #[test]
    fn nearer_cluster_takes_excess_up_to_its_demand() {
        let supply = vec![supply("FROM", 10)];
        let clusters = vec![cluster("FAR", 70, 70), cluster("NEAR", 4, 70)];
        let tariffs = HashMap::from([
            tariff("FROM", "FAR", 900, 9_000.0),
            tariff("FROM", "NEAR", 100, 1_000.0),
        ]);
        let got = solve_spray_assignment(
            &[10],
            &supply,
            &clusters,
            &tariffs,
            &ConventionIndex::disabled(),
            &BusinessRules::default(),
        );
        let near: i32 = got.iter().filter(|a| a.c_idx == 1).map(|a| a.quantity).sum();
        let far: i32 = got.iter().filter(|a| a.c_idx == 0).map(|a| a.quantity).sum();
        // Спрос ближнего кластера 4 < минимума партии — туда не шлём.
        assert_eq!(near, 0);
        assert_eq!(far, 10);
    }

    #[test]
    fn batch_below_minimum_stays_unassigned() {
        let supply = vec![supply("FROM", 4)];
        let clusters = vec![cluster("NEAR", 70, 70)];
        let tariffs = HashMap::from([tariff("FROM", "NEAR", 100, 1_000.0)]);
        let got = solve_spray_assignment(
            &[4],
            &supply,
            &clusters,
            &tariffs,
            &ConventionIndex::disabled(),
            &BusinessRules::default(),
        );
        assert!(got.is_empty());
    }

    #[test]
    fn several_origins_may_fill_one_station_to_the_minimum() {
        let supply = vec![supply("A", 3), supply("B", 3)];
        let clusters = vec![cluster("SPRAY", 70, 70)];
        let tariffs = HashMap::from([
            tariff("A", "SPRAY", 100, 1_000.0),
            tariff("B", "SPRAY", 120, 1_200.0),
        ]);
        let got = solve_spray_assignment(
            &[3, 3],
            &supply,
            &clusters,
            &tariffs,
            &ConventionIndex::disabled(),
            &BusinessRules::default(),
        );
        let total: i32 = got.iter().map(|a| a.quantity).sum();
        assert_eq!(total, 6);
    }
}

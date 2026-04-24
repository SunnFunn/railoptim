//! Диагностика остатка предложения (`excess_supply`) после MIP/ALNS.
//!
//! Цель — для каждого узла предложения с `car_count > sent` объяснить, почему
//! решатель не распределил оставшиеся вагоны. На реальных данных типовые причины
//! разделены по категориям (см. [`ExcessCause`]) — это позволяет быстро понять,
//! нужно ли править входные данные, релаксировать `MIN_BATCH` или поднимать
//! `PENALTY_COST`.
//!
//! Функция не меняет состояние — только печатает отчёт в stdout.

use std::collections::{BTreeMap, HashMap};

use crate::node::SupplyNode;

use super::lp::PENALTY_COST;
use super::model::{MIN_BATCH_FROM_MASS_STATION, TaskArc};
use crate::node::DemandNode;

/// Категория причины, по которой вагоны узла предложения остались нераспределёнными.
#[derive(Debug)]
enum ExcessCause {
    /// Из узла вовсе нет допустимых дуг (нет тарифа, несовместим тип вагона, …).
    NoArcs,

    /// Все дуги из узла идут в спросы, которые уже полностью закрыты другими
    /// назначениями. Свободной «полезной работы» для этих вагонов нет.
    AllTargetsCovered { arcs_count: usize },

    /// Все дуги с доступным (не полностью закрытым) спросом упираются в
    /// `MIN_BATCH`: пара `(supply_station, demand_station)` — пара массовой
    /// выгрузки, и текущий поток в ней `< MIN_BATCH`, а суммарный потенциал
    /// (текущий поток + доступное) — тоже меньше минимального батча.
    MinBatchDeadlock {
        pairs: Vec<(String, String, i32, i32)>, // (ss, ds, current_flow, potential_add)
    },

    /// Есть feasible-дуги с доступным спросом, их минимальная стоимость выше
    /// `2 * PENALTY_COST` (штраф за excess + штраф за unmet). MIP математически
    /// правильно предпочёл штраф вместо дорогой маршрутизации.
    PenaltyCheaperThanArcs {
        feasible_arcs_count: usize,
        min_arc_cost_per_wagon: f64,
    },

    /// Есть feasible-дуги дешевле `2 * PENALTY`, но MIP их не задействовал.
    /// Редкий случай — возможно, использование этих дуг нарушит `MIN_BATCH`
    /// на соседних парах (каскадный эффект) или упирается в ёмкость Wash.
    UnexpectedNotUsed {
        feasible_arcs_count: usize,
        min_arc_cost_per_wagon: f64,
    },
}

/// Печатает отчёт по нераспределённым вагонам предложения.
///
/// Берёт текущее решение `arc_vals` (в том же порядке, что `arcs`) и показывает
/// по каждому узлу с остатком: станция, тип вагона, ЕТСНГ, period,
/// mass_unloading-флаг, остаток, и главную причину.
pub fn diagnose_excess_supply(
    arcs: &[TaskArc],
    arc_vals: &[f64],
    supply: &[SupplyNode],
    demand: &[DemandNode],
) {
    if arcs.len() != arc_vals.len() {
        eprintln!(
            "diagnose_excess_supply: размеры arcs ({}) и arc_vals ({}) не совпадают — диагностика пропущена.",
            arcs.len(), arc_vals.len()
        );
        return;
    }

    // 1. Агрегируем потоки.
    let mut sent = vec![0_i32; supply.len()];
    let mut recv = vec![0_i32; demand.len()];
    for (arc, &q) in arcs.iter().zip(arc_vals.iter()) {
        let qi = q.round() as i32;
        if qi <= 0 { continue; }
        sent[arc.s_idx] += qi;
        recv[arc.d_idx] += qi;
    }
    let rem_supply: Vec<i32> = supply.iter().enumerate()
        .map(|(i, s)| s.car_count - sent[i])
        .collect();
    let rem_demand: Vec<i32> = demand.iter().enumerate()
        .map(|(i, d)| d.car_count - recv[i])
        .collect();

    // 2. Текущий поток по mass-unloading парам (ss, ds).
    let mut pair_flow: HashMap<(String, String), i32> = HashMap::new();
    for (arc, &q) in arcs.iter().zip(arc_vals.iter()) {
        if !arc.is_mass_unloading { continue; }
        let qi = q.round() as i32;
        if qi <= 0 { continue; }
        *pair_flow
            .entry((arc.supply_station_code.clone(), arc.demand_station_code.clone()))
            .or_insert(0) += qi;
    }

    // 3. Узлы с excess.
    let excess_nodes: Vec<usize> = rem_supply
        .iter()
        .enumerate()
        .filter(|&(_, &r)| r > 0)
        .map(|(i, _)| i)
        .collect();

    if excess_nodes.is_empty() {
        println!("--- ДИАГНОСТИКА EXCESS SUPPLY ---");
        println!("Нераспределённых вагонов нет — все узлы предложения закрыты.");
        println!("---------------------------------");
        return;
    }

    let total_excess: i32 = excess_nodes.iter().map(|&i| rem_supply[i]).sum();
    println!(
        "--- ДИАГНОСТИКА EXCESS SUPPLY ({} ваг. в {} узлах) ---",
        total_excess, excess_nodes.len()
    );

    // Индекс дуг по узлу предложения.
    let mut arcs_by_supply: HashMap<usize, Vec<usize>> = HashMap::new();
    for arc in arcs {
        arcs_by_supply.entry(arc.s_idx).or_default().push(arc.arc_id);
    }

    // Счётчики причин — печатаются в конце.
    let mut cause_stats: BTreeMap<&'static str, (usize, i32)> = BTreeMap::new();
    let add_stat = |key: &'static str, rem: i32, stats: &mut BTreeMap<&'static str, (usize, i32)>| {
        let e = stats.entry(key).or_insert((0, 0));
        e.0 += 1; e.1 += rem;
    };

    for &s_idx in &excess_nodes {
        let s = &supply[s_idx];
        let rem = rem_supply[s_idx];

        let node_arcs = arcs_by_supply
            .get(&s_idx)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        // Разбиваем дуги по статусу.
        let mut feasible_available: Vec<usize> = Vec::new(); // спрос есть, MIN_BATCH не блокирует
        let mut min_batch_blocked: Vec<usize>  = Vec::new();
        let mut target_covered: Vec<usize>     = Vec::new(); // спрос уже закрыт

        let mut min_arc_cost_feasible = f64::INFINITY;
        let mut min_batch_pairs: HashMap<(String, String), (i32, i32)> = HashMap::new();

        for &arc_id in node_arcs {
            let arc = &arcs[arc_id];
            let d_rem = rem_demand[arc.d_idx];
            if d_rem <= 0 {
                target_covered.push(arc_id);
                continue;
            }
            if arc.is_mass_unloading {
                let key = (arc.supply_station_code.clone(), arc.demand_station_code.clone());
                let flow = pair_flow.get(&key).copied().unwrap_or(0);
                let add_potential = rem.min(d_rem);
                // Пара блокируется MIN_BATCH если:
                //   - текущий поток = 0 и потенциал добавления < MIN_BATCH (нельзя «запустить» пару);
                //   - или поток > 0, но < MIN_BATCH (MIP поставил y_pair=0 и запретил пару целиком).
                let blocked = if flow == 0 {
                    add_potential < MIN_BATCH_FROM_MASS_STATION
                } else {
                    flow < MIN_BATCH_FROM_MASS_STATION
                };
                if blocked {
                    min_batch_blocked.push(arc_id);
                    min_batch_pairs
                        .entry(key)
                        .and_modify(|e| { e.0 = flow; e.1 = e.1.max(add_potential); })
                        .or_insert((flow, add_potential));
                    continue;
                }
            }
            feasible_available.push(arc_id);
            if arc.cost < min_arc_cost_feasible {
                min_arc_cost_feasible = arc.cost;
            }
        }

        // Категоризация.
        let cause = if node_arcs.is_empty() {
            ExcessCause::NoArcs
        } else if feasible_available.is_empty() && min_batch_blocked.is_empty() {
            ExcessCause::AllTargetsCovered { arcs_count: node_arcs.len() }
        } else if feasible_available.is_empty() {
            ExcessCause::MinBatchDeadlock {
                pairs: min_batch_pairs
                    .into_iter()
                    .map(|((ss, ds), (f, p))| (ss, ds, f, p))
                    .collect(),
            }
        } else if min_arc_cost_feasible >= 2.0 * PENALTY_COST {
            ExcessCause::PenaltyCheaperThanArcs {
                feasible_arcs_count: feasible_available.len(),
                min_arc_cost_per_wagon: min_arc_cost_feasible,
            }
        } else {
            ExcessCause::UnexpectedNotUsed {
                feasible_arcs_count: feasible_available.len(),
                min_arc_cost_per_wagon: min_arc_cost_feasible,
            }
        };

        println!(
            "  [s_idx {:>4}] {} | тип={} | ЕТСНГ={} | period={} | mass_unload={} | осталось {} из {} ваг.",
            s_idx,
            s.station_to,
            s.car_type.as_deref().unwrap_or("—"),
            s.etsng.as_deref().unwrap_or("—"),
            s.supply_period,
            s.is_mass_unloading,
            rem,
            s.car_count,
        );
        match &cause {
            ExcessCause::NoArcs => {
                println!("    ПРИЧИНА: нет ни одной допустимой дуги (нет тарифа, несовм. тип вагона или грязный груз).");
                add_stat("no_arcs", rem, &mut cause_stats);
            }
            ExcessCause::AllTargetsCovered { arcs_count } => {
                println!(
                    "    ПРИЧИНА: все спросы-адресаты уже полностью закрыты (допустимых дуг {}, но rem_demand=0 на всех).",
                    arcs_count
                );
                add_stat("targets_covered", rem, &mut cause_stats);
            }
            ExcessCause::MinBatchDeadlock { pairs } => {
                println!(
                    "    ПРИЧИНА: MIN_BATCH-тупик ({} пар). Все дуги с доступным спросом — в пары массовой выгрузки с потоком <{}:",
                    pairs.len(), MIN_BATCH_FROM_MASS_STATION
                );
                for (ss, ds, flow, potential) in pairs.iter().take(5) {
                    println!(
                        "      · ({ss} → {ds}): текущий поток {flow} ваг., макс. добавим {potential} (порог {MIN_BATCH_FROM_MASS_STATION})",
                    );
                }
                if pairs.len() > 5 {
                    println!("      · ...ещё {} пар", pairs.len() - 5);
                }
                add_stat("min_batch_deadlock", rem, &mut cause_stats);
            }
            ExcessCause::PenaltyCheaperThanArcs {
                feasible_arcs_count,
                min_arc_cost_per_wagon,
            } => {
                println!(
                    "    ПРИЧИНА: feasible-дуги есть ({} шт.), но мин. стоимость {:.0} руб./ваг. ≥ 2×PENALTY ({:.0} руб./ваг.). Штраф оптимальнее маршрута.",
                    feasible_arcs_count,
                    min_arc_cost_per_wagon,
                    2.0 * PENALTY_COST,
                );
                add_stat("penalty_cheaper", rem, &mut cause_stats);
            }
            ExcessCause::UnexpectedNotUsed {
                feasible_arcs_count,
                min_arc_cost_per_wagon,
            } => {
                println!(
                    "    ПРИЧИНА: feasible-дуги есть ({} шт., мин. стоимость {:.0} руб./ваг. < 2×PENALTY), но MIP их не задействовал.",
                    feasible_arcs_count, min_arc_cost_per_wagon,
                );
                println!("             Вероятно, каскадный эффект: использование дуги сломает MIN_BATCH в соседней паре.");
                add_stat("unexpected", rem, &mut cause_stats);
            }
        }
    }

    // Сводка.
    println!();
    println!("  СВОДКА ПО ПРИЧИНАМ:");
    for (cause, (n_nodes, n_cars)) in &cause_stats {
        let label = match *cause {
            "no_arcs"            => "нет допустимых дуг",
            "targets_covered"    => "все адресаты закрыты",
            "min_batch_deadlock" => "MIN_BATCH-тупик",
            "penalty_cheaper"    => "штраф < стоимости дуг",
            "unexpected"         => "дуги есть, но не использованы",
            _                    => cause,
        };
        println!("    {:30} узлов: {:>3}, вагонов: {:>4}", label, n_nodes, n_cars);
    }
    println!("---------------------------------");
}

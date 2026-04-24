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

use crate::node::{DemandNode, DemandPurpose, SupplyNode};

use super::lp::PENALTY_COST;
use super::model::{MIN_BATCH_FROM_MASS_STATION, TaskArc};

/// Категория причины, по которой вагоны узла предложения остались нераспределёнными.
///
/// ВАЖНО: excess_supply в модели штрафа **не имеет** (dummy_demand cost=0), а
/// unmet_demand штрафуется `PENALTY_COST` (только для Load-спроса). Поэтому:
///   * отправка в Load-демы выгодна, если `arc.cost < PENALTY_COST`
///     (экономия = PENALTY - arc.cost на вагон);
///   * отправка в Wash-демы **никогда не выгодна** для снижения obj, потому что
///     Wash не имеет штрафа за незаполнение, а arc всегда имеет ненулевую цену.
#[derive(Debug)]
enum ExcessCause {
    /// Из узла вовсе нет допустимых дуг (нет тарифа, несовместим тип вагона, …).
    NoArcs,

    /// Все Load-дуги из узла идут в спросы, которые уже полностью закрыты
    /// другими назначениями (rem_demand=0). Wash-дуги игнорируем — они
    /// оптимизатору невыгодны по построению модели.
    AllTargetsCovered {
        load_arcs: usize,
        wash_arcs: usize,
    },

    /// У узла с доступным спросом остались **только Wash-дуги**. MIP корректно
    /// оставил вагоны в excess: Wash не имеет штрафа, и отправка только
    /// увеличила бы стоимость.
    OnlyWashAvailable {
        wash_arcs: usize,
        min_arc_cost_per_wagon: f64,
    },

    /// Все Load-дуги с доступным спросом упираются в `MIN_BATCH`: пара
    /// `(supply_station, demand_station)` — пара массовой выгрузки, и текущий
    /// поток в ней `< MIN_BATCH`, а суммарный потенциал тоже меньше.
    MinBatchDeadlock {
        pairs: Vec<(String, String, i32, i32)>, // (ss, ds, current_flow, potential_add)
    },

    /// Есть feasible Load-дуги с доступным спросом, их минимальная стоимость
    /// выше `PENALTY_COST`. MIP математически правильно предпочёл штраф unmet
    /// вместо дорогой маршрутизации.
    PenaltyCheaperThanArcs {
        feasible_arcs_count: usize,
        min_arc_cost_per_wagon: f64,
    },

    /// Есть feasible Load-дуги дешевле `PENALTY`, но MIP их не задействовал.
    /// Действительно подозрительный случай — обычно означает каскадный
    /// эффект `MIN_BATCH` на соседних парах.
    UnexpectedNotUsed {
        feasible_arcs_count: usize,
        min_arc_cost_per_wagon: f64,
        top_arcs: Vec<(String, f64, i32)>, // (demand_station, cost, d_rem)
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

        // Разбиваем дуги по статусу. Важно: Wash-дуги рассматриваем отдельно —
        // их отправка не снижает unmet, поэтому для MIP они «не полезны».
        let mut load_feasible: Vec<usize> = Vec::new(); // Load-демы, rem>0, MIN_BATCH не блок.
        let mut wash_feasible: Vec<usize> = Vec::new(); // Wash-демы с rem>0 (информационно).
        let mut load_min_batch_blocked: Vec<usize> = Vec::new();
        let mut load_target_covered = 0_usize;
        let mut wash_target_covered = 0_usize;

        let mut min_arc_cost_load = f64::INFINITY;
        let mut min_arc_cost_wash = f64::INFINITY;
        let mut min_batch_pairs: HashMap<(String, String), (i32, i32)> = HashMap::new();

        for &arc_id in node_arcs {
            let arc = &arcs[arc_id];
            let d = &demand[arc.d_idx];
            let d_rem = rem_demand[arc.d_idx];
            let is_wash = d.purpose == DemandPurpose::Wash;

            if d_rem <= 0 {
                if is_wash { wash_target_covered += 1; } else { load_target_covered += 1; }
                continue;
            }

            if is_wash {
                wash_feasible.push(arc_id);
                if arc.cost < min_arc_cost_wash { min_arc_cost_wash = arc.cost; }
                continue;
            }

            // Load-дуга с rem>0: проверяем MIN_BATCH (только для mass_unloading).
            if arc.is_mass_unloading {
                let key = (arc.supply_station_code.clone(), arc.demand_station_code.clone());
                let flow = pair_flow.get(&key).copied().unwrap_or(0);
                let add_potential = rem.min(d_rem);
                let blocked = if flow == 0 {
                    add_potential < MIN_BATCH_FROM_MASS_STATION
                } else {
                    flow < MIN_BATCH_FROM_MASS_STATION
                };
                if blocked {
                    load_min_batch_blocked.push(arc_id);
                    min_batch_pairs
                        .entry(key)
                        .and_modify(|e| { e.0 = flow; e.1 = e.1.max(add_potential); })
                        .or_insert((flow, add_potential));
                    continue;
                }
            }
            load_feasible.push(arc_id);
            if arc.cost < min_arc_cost_load { min_arc_cost_load = arc.cost; }
        }

        // Категоризация. Важно: дешёвая Wash-дуга сама по себе не оправдывает
        // отправку — учитываем только Load-дуги.
        let cause = if node_arcs.is_empty() {
            ExcessCause::NoArcs
        } else if load_feasible.is_empty() && load_min_batch_blocked.is_empty() {
            // Нет ни одного Load-направления с доступным спросом. Остались либо
            // Wash-дуги, либо всё закрыто.
            if !wash_feasible.is_empty() {
                ExcessCause::OnlyWashAvailable {
                    wash_arcs: wash_feasible.len(),
                    min_arc_cost_per_wagon: min_arc_cost_wash,
                }
            } else {
                ExcessCause::AllTargetsCovered {
                    load_arcs: load_target_covered,
                    wash_arcs: wash_target_covered,
                }
            }
        } else if load_feasible.is_empty() {
            ExcessCause::MinBatchDeadlock {
                pairs: min_batch_pairs
                    .into_iter()
                    .map(|((ss, ds), (f, p))| (ss, ds, f, p))
                    .collect(),
            }
        } else if min_arc_cost_load >= PENALTY_COST {
            ExcessCause::PenaltyCheaperThanArcs {
                feasible_arcs_count: load_feasible.len(),
                min_arc_cost_per_wagon: min_arc_cost_load,
            }
        } else {
            // Собираем ТОП-3 самых дешёвых Load-дуги для детальной отладки.
            let mut top: Vec<(String, f64, i32)> = load_feasible
                .iter()
                .map(|&aid| {
                    let arc = &arcs[aid];
                    (
                        demand[arc.d_idx].station_name.clone(),
                        arc.cost,
                        rem_demand[arc.d_idx],
                    )
                })
                .collect();
            top.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            top.truncate(3);
            ExcessCause::UnexpectedNotUsed {
                feasible_arcs_count: load_feasible.len(),
                min_arc_cost_per_wagon: min_arc_cost_load,
                top_arcs: top,
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
            ExcessCause::AllTargetsCovered { load_arcs, wash_arcs } => {
                println!(
                    "    ПРИЧИНА: все Load-спросы уже закрыты (Load-дуг {}, Wash-дуг {}; везде rem_demand=0).",
                    load_arcs, wash_arcs,
                );
                add_stat("targets_covered", rem, &mut cause_stats);
            }
            ExcessCause::OnlyWashAvailable { wash_arcs, min_arc_cost_per_wagon } => {
                println!(
                    "    ПРИЧИНА: доступны только Wash-дуги ({} шт., мин. стоимость {:.0} руб./ваг.). В модели Wash не имеет штрафа за незаполнение,",
                    wash_arcs, min_arc_cost_per_wagon,
                );
                println!("             а excess_supply бесплатный — отправка только увеличила бы obj, MIP корректно оставил вагоны в остатке.");
                add_stat("only_wash", rem, &mut cause_stats);
            }
            ExcessCause::MinBatchDeadlock { pairs } => {
                println!(
                    "    ПРИЧИНА: MIN_BATCH-тупик ({} пар). Все Load-дуги с доступным спросом — пары массовой выгрузки с потоком <{}:",
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
                    "    ПРИЧИНА: Load-дуги есть ({} шт.), но мин. стоимость {:.0} руб./ваг. ≥ PENALTY ({:.0}). Штраф unmet дешевле маршрута.",
                    feasible_arcs_count,
                    min_arc_cost_per_wagon,
                    PENALTY_COST,
                );
                add_stat("penalty_cheaper", rem, &mut cause_stats);
            }
            ExcessCause::UnexpectedNotUsed {
                feasible_arcs_count,
                min_arc_cost_per_wagon,
                top_arcs,
            } => {
                println!(
                    "    ПРИЧИНА: Load-дуги есть ({} шт., мин. стоимость {:.0} руб./ваг. < PENALTY {:.0}), но MIP их не задействовал.",
                    feasible_arcs_count, min_arc_cost_per_wagon, PENALTY_COST,
                );
                println!("             Вероятно, каскадный эффект MIN_BATCH на соседних парах. ТОП-3 самых дешёвых:");
                for (ds, cost, d_rem) in top_arcs {
                    println!("      · → {ds}: cost={:.0} руб./ваг., rem_demand={}", cost, d_rem);
                }
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
            "targets_covered"    => "все Load-адресаты закрыты",
            "only_wash"          => "доступны только Wash-дуги",
            "min_batch_deadlock" => "MIN_BATCH-тупик",
            "penalty_cheaper"    => "штраф < стоимости дуг",
            "unexpected"         => "Load-дуги есть, но не использованы",
            _                    => cause,
        };
        println!("    {:35} узлов: {:>3}, вагонов: {:>4}", label, n_nodes, n_cars);
    }
    println!("---------------------------------");
}

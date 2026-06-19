//! Диагностика остатка предложения (`excess_supply`) после MIP/ALNS.
//!
//! Цель — для каждого узла предложения с `car_count > sent` объяснить, почему
//! решатель не распределил оставшиеся вагоны. На реальных данных типовые причины
//! разделены по категориям (см. [`ExcessCause`]) — это позволяет быстро понять,
//! нужно ли править входные данные, релаксировать `MIN_BATCH` или поднимать
//! `PENALTY_UNMET`.
//!
//! Функция не меняет состояние — только печатает отчёт в stdout.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::node::{DemandNode, DemandPurpose, SupplyNode, TariffNode};

use super::lp::PENALTY_UNMET;
use super::model::{
    classify_pair, wash_route_min_cost_by_station, DmziIndex, DmziLimits, PairKey, PairOutcome,
    TaskArc, MIN_BATCH_FROM_MASS_STATION, MIN_BATCH_TO_MIDDLE_DEMAND_STATION,
    MIN_BATCH_TO_ROUTE_DEMAND_STATION,
};

/// Категория причины, по которой вагоны узла предложения остались нераспределёнными.
///
/// ВАЖНО: excess_supply штрафуется `PENALTY_EXCESS` (5 000 руб., ниже мин. тарифа),
/// unmet_demand штрафуется `PENALTY_UNMET` (1 000 000 руб., выше макс. тарифа).
/// Поэтому:
///   * отправка в Load-демы выгодна, если `arc.cost < PENALTY_UNMET`
///     (экономия = PENALTY_UNMET - arc.cost на вагон);
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

    /// Все Load-дуги с доступным спросом упираются в ограничение минимальной партии:
    /// пара `(supply_station, demand_station)` имеет `pair_min_batch > 0` (массовая
    /// выгрузка или средние станции), текущий поток в ней `< порога`, а суммарный
    /// потенциал тоже меньше.
    MinBatchDeadlock {
        pairs: Vec<(String, String, i32, i32, i32)>, // (ss, ds, current_flow, potential_add, min_batch)
    },

    /// Все Load-дуги с доступным спросом упираются в исчерпанную квоту ДМЗИ:
    /// бакет `(дорога погрузки, период предложения)` уже использован полностью.
    DmziQuotaExhausted {
        buckets: Vec<(String, u8, i32, i32)>, // (дорога, период, used, limit)
    },

    /// Есть feasible Load-дуги с доступным спросом, их минимальная стоимость
    /// выше `PENALTY_UNMET`. MIP математически правильно предпочёл штраф unmet
    /// вместо дорогой маршрутизации.
    PenaltyCheaperThanArcs {
        feasible_arcs_count: usize,
        min_arc_cost_per_wagon: f64,
    },

    /// Есть feasible Load-дуги дешевле `PENALTY_UNMET`, но MIP их не задействовал.
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
    dmzi_limits: Option<&DmziLimits>,
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

    // 2. Текущий поток по группам с ограничением минимальной партии (pair_key).
    let mut pair_flow: HashMap<PairKey, i32> = HashMap::new();
    for (arc, &q) in arcs.iter().zip(arc_vals.iter()) {
        if !arc.has_pair_min_batch() { continue; }
        let qi = q.round() as i32;
        if qi <= 0 { continue; }
        *pair_flow.entry(arc.pair_key()).or_insert(0) += qi;
    }

    // 2а. Квоты ДМЗИ: индекс бакетов и их текущее использование решением.
    let dmzi: Option<(DmziIndex, Vec<i32>)> = dmzi_limits
        .filter(|l| !l.is_empty())
        .map(|l| {
            let idx = DmziIndex::build(arcs, supply, demand, l);
            let used = idx.usage_from_arc_vals(arc_vals);
            (idx, used)
        });

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
        let mut load_dmzi_blocked: Vec<usize> = Vec::new();
        let mut load_target_covered = 0_usize;
        let mut wash_target_covered = 0_usize;

        let mut min_arc_cost_load = f64::INFINITY;
        let mut min_arc_cost_wash = f64::INFINITY;
        // pair_key (порог — третий элемент) → (текущий поток, макс. потенциал добавления).
        let mut min_batch_pairs: HashMap<PairKey, (i32, i32)> = HashMap::new();
        // Индексы исчерпанных бакетов ДМЗИ, блокирующих дуги узла.
        let mut dmzi_buckets: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();

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

            // Квота ДМЗИ — жёсткий потолок дороги, проверяется ПЕРВОЙ. Иначе дуга,
            // одновременно «ниже партии» и на исчерпанной дороге, ложно
            // приписывается MIN_BATCH, хотя реально её держит инфраструктура.
            if let Some((idx, used)) = &dmzi {
                if let Some(b) = idx.arc_bucket[arc_id] {
                    if used[b] >= idx.buckets[b].1 {
                        load_dmzi_blocked.push(arc_id);
                        dmzi_buckets.insert(b);
                        continue;
                    }
                }
            }

            // Ограничение минимальной партии (на дороге ещё есть свободная квота ДМЗИ,
            // значит держит именно партия, а не инфраструктура).
            if arc.has_pair_min_batch() {
                let b = arc.pair_min_batch;
                let key = arc.pair_key();
                let flow = pair_flow.get(&key).copied().unwrap_or(0);
                let add_potential = rem.min(d_rem);
                let blocked = if flow == 0 {
                    add_potential < b
                } else {
                    flow < b
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
        } else if load_feasible.is_empty()
            && load_min_batch_blocked.is_empty()
            && load_dmzi_blocked.is_empty()
        {
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
        } else if load_feasible.is_empty() && !load_min_batch_blocked.is_empty() {
            ExcessCause::MinBatchDeadlock {
                pairs: min_batch_pairs
                    .into_iter()
                    .map(|((ss, ds, b), (f, p))| (ss, ds, f, p, b))
                    .collect(),
            }
        } else if load_feasible.is_empty() {
            // Остались только дуги, заблокированные квотами ДМЗИ.
            let (idx, used) = dmzi.as_ref().expect("dmzi_buckets непусто только при Some");
            ExcessCause::DmziQuotaExhausted {
                buckets: dmzi_buckets
                    .iter()
                    .map(|&b| {
                        let ((rw, period), limit) = &idx.buckets[b];
                        (rw.clone(), *period, used[b], *limit)
                    })
                    .collect(),
            }
        } else if min_arc_cost_load >= PENALTY_UNMET {
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
                    "    ПРИЧИНА: MIN_BATCH-тупик ({} пар). Все Load-дуги с доступным спросом — пары с потоком ниже порога партии:",
                    pairs.len()
                );
                for (ss, ds, flow, potential, min_batch) in pairs.iter().take(5) {
                    println!(
                        "      · ({ss} → {ds}): текущий поток {flow} ваг., макс. добавим {potential} (порог {min_batch})",
                    );
                }
                if pairs.len() > 5 {
                    println!("      · ...ещё {} пар", pairs.len() - 5);
                }
                add_stat("min_batch_deadlock", rem, &mut cause_stats);
            }
            ExcessCause::DmziQuotaExhausted { buckets } => {
                println!(
                    "    ПРИЧИНА: квота ДМЗИ исчерпана — все Load-дуги с доступным спросом идут на дороги с выбранным лимитом:"
                );
                for (rw, period, used, limit) in buckets.iter().take(5) {
                    println!(
                        "      · дорога {rw}, период {period}: использовано {used} из {limit} ваг.",
                    );
                }
                if buckets.len() > 5 {
                    println!("      · ...ещё {} бакетов", buckets.len() - 5);
                }
                add_stat("dmzi_quota", rem, &mut cause_stats);
            }
            ExcessCause::PenaltyCheaperThanArcs {
                feasible_arcs_count,
                min_arc_cost_per_wagon,
            } => {
                println!(
                    "    ПРИЧИНА: Load-дуги есть ({} шт.), но мин. стоимость {:.0} руб./ваг. ≥ PENALTY_UNMET ({:.0}). Штраф unmet дешевле маршрута.",
                    feasible_arcs_count,
                    min_arc_cost_per_wagon,
                    PENALTY_UNMET,
                );
                add_stat("penalty_cheaper", rem, &mut cause_stats);
            }
            ExcessCause::UnexpectedNotUsed {
                feasible_arcs_count,
                min_arc_cost_per_wagon,
                top_arcs,
            } => {
                println!(
                    "    ПРИЧИНА: Load-дуги есть ({} шт., мин. стоимость {:.0} руб./ваг. < PENALTY_UNMET {:.0}), но MIP их не задействовал.",
                    feasible_arcs_count, min_arc_cost_per_wagon, PENALTY_UNMET,
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
            "dmzi_quota"         => "квота ДМЗИ исчерпана",
            "penalty_cheaper"    => "штраф < стоимости дуг",
            "unexpected"         => "Load-дуги есть, но не использованы",
            _                    => cause,
        };
        println!("    {:35} узлов: {:>3}, вагонов: {:>4}", label, n_nodes, n_cars);
    }
    println!("---------------------------------");
}

/// Категория причины, по которой узел спроса на погрузку (`Load`) остался незакрытым.
///
/// Зеркальна [`ExcessCause`]: там разбирается, почему вагон **стоит**, здесь —
/// почему заявку **некем закрыть**. Ключевое деление: [`NoFeasibleArcs`] —
/// структурно недостижимо (нет дуг, никакая настройка солвера не поможет);
/// остальные причины означают, что заявка закрываема в принципе, но упирается
/// в партийность, квоту ДМЗИ, конкуренцию за предложение или экономику штрафа.
///
/// [`NoFeasibleArcs`]: UnmetCause::NoFeasibleArcs
#[derive(Debug)]
enum UnmetCause {
    /// Ни одной допустимой дуги в узел нет — закрыть текущими данными невозможно.
    /// Разбивка показывает, на каком жёстком фильтре отброшены пары со **всеми**
    /// узлами предложения (сумма = `supply_nodes_total`).
    NoFeasibleArcs {
        supply_nodes_total: usize,
        no_tariff: usize,
        bad_type: usize,
        dirty_etsng: usize,
        bad_period: usize,
    },

    /// Дуги есть, но все узлы-источники предложения исчерпаны (`rem_supply == 0`):
    /// совместимые вагоны ушли на другие (более выгодные) заявки. С учётом
    /// глобального профицита это прямая конкуренция за ограниченное совместимое предложение.
    AllSourcesExhausted {
        arc_count: usize,
    },

    /// Все дуги со свободным предложением упираются в ограничение минимальной партии.
    MinBatchDeadlock {
        pairs: Vec<(String, String, i32, i32, i32)>, // (ss, ds, current_flow, potential_add, min_batch)
    },

    /// Все дуги со свободным предложением упираются в исчерпанную квоту ДМЗИ.
    DmziQuotaExhausted {
        buckets: Vec<(String, u8, i32, i32)>, // (дорога, период, used, limit)
    },

    /// Есть дуги со свободным предложением, но их мин. стоимость ≥ `PENALTY_UNMET`:
    /// MIP правильно предпочёл штраф unmet дорогой маршрутизации (закрываемо
    /// физически, но не экономически при текущем штрафе).
    PenaltyCheaperThanArcs {
        feasible_arcs_count: usize,
        min_arc_cost_per_wagon: f64,
    },

    /// Есть дуги со свободным предложением дешевле `PENALTY_UNMET`, но не задействованы.
    /// Подозрительно при доказанном оптимуме — обычно каскадный эффект `MIN_BATCH`
    /// на соседних парах.
    UnexpectedNotUsed {
        feasible_arcs_count: usize,
        min_arc_cost_per_wagon: f64,
        top_sources: Vec<(String, f64, i32)>, // (supply_station, cost, rem_supply)
    },
}

/// Печатает отчёт по незакрытому спросу на погрузку (`DemandPurpose::Load`).
///
/// Зеркало [`diagnose_excess_supply`]: для каждого узла спроса с `rem_demand > 0`
/// объясняет, почему ни один вагон не дошёл. Главный вывод — деление остатка на
/// «структурно недостижимо» (нет дуг) и «потенциально закрываемо» (партия / ДМЗИ /
/// конкуренция), что показывает реальный потолок покрытия.
///
/// `tariffs` / `wash_codes` / `no_cleaning_roads` / `wash_tariffs` нужны для
/// структурной разбивки узлов без дуг через [`classify_pair`].
#[allow(clippy::too_many_arguments)]
pub fn diagnose_unmet_demand(
    arcs: &[TaskArc],
    arc_vals: &[f64],
    supply: &[SupplyNode],
    demand: &[DemandNode],
    tariffs: &[TariffNode],
    wash_codes: &HashSet<String>,
    no_cleaning_roads: &HashSet<String>,
    washed_empty_codes: &HashSet<String>,
    wash_tariffs: &HashMap<(String, String), TariffNode>,
    dmzi_limits: Option<&DmziLimits>,
) {
    if arcs.len() != arc_vals.len() {
        eprintln!(
            "diagnose_unmet_demand: размеры arcs ({}) и arc_vals ({}) не совпадают — диагностика пропущена.",
            arcs.len(), arc_vals.len()
        );
        return;
    }

    // 1. Агрегируем потоки и считаем остатки.
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

    // 2. Текущий поток по группам с ограничением минимальной партии.
    let mut pair_flow: HashMap<PairKey, i32> = HashMap::new();
    for (arc, &q) in arcs.iter().zip(arc_vals.iter()) {
        if !arc.has_pair_min_batch() { continue; }
        let qi = q.round() as i32;
        if qi <= 0 { continue; }
        *pair_flow.entry(arc.pair_key()).or_insert(0) += qi;
    }

    // 2а. Квоты ДМЗИ: индекс бакетов и их использование решением.
    let dmzi: Option<(DmziIndex, Vec<i32>)> = dmzi_limits
        .filter(|l| !l.is_empty())
        .map(|l| {
            let idx = DmziIndex::build(arcs, supply, demand, l);
            let used = idx.usage_from_arc_vals(arc_vals);
            (idx, used)
        });

    // 3. Незакрытые узлы спроса на погрузку.
    let unmet_nodes: Vec<usize> = demand.iter().enumerate()
        .filter(|(i, d)| d.purpose == DemandPurpose::Load && rem_demand[*i] > 0)
        .map(|(i, _)| i)
        .collect();

    if unmet_nodes.is_empty() {
        println!("--- ДИАГНОСТИКА UNMET DEMAND ---");
        println!("Незакрытого спроса на погрузку нет — все Load-заявки удовлетворены.");
        println!("--------------------------------");
        return;
    }

    let total_unmet: i32 = unmet_nodes.iter().map(|&i| rem_demand[i]).sum();
    println!(
        "--- ДИАГНОСТИКА UNMET DEMAND ({} ваг. в {} узлах) ---",
        total_unmet, unmet_nodes.len()
    );

    // Индекс дуг по узлу спроса.
    let mut arcs_by_demand: HashMap<usize, Vec<usize>> = HashMap::new();
    for arc in arcs {
        arcs_by_demand.entry(arc.d_idx).or_default().push(arc.arc_id);
    }

    // Индекс тарифов погрузки — для структурной разбивки узлов без дуг.
    let tariff_index: HashMap<(&str, &str), &TariffNode> = tariffs
        .iter()
        .map(|t| ((t.station_from_code.as_str(), t.station_to_code.as_str()), t))
        .collect();

    // Порог «cap» промывочного маршрута по станции образования (как в build_task_arcs).
    let wash_min_cost = wash_route_min_cost_by_station(wash_tariffs);

    let mut cause_stats: BTreeMap<&'static str, (usize, i32)> = BTreeMap::new();
    let add_stat = |key: &'static str, rem: i32, stats: &mut BTreeMap<&'static str, (usize, i32)>| {
        let e = stats.entry(key).or_insert((0, 0));
        e.0 += 1; e.1 += rem;
    };

    // Разбивка MIN_BATCH-узлов по самому низкому достижимому порогу партии:
    // показывает, какому классу станций смягчение MIN_BATCH реально помогло бы.
    // Учитываются только дуги на дорогах со свободной квотой ДМЗИ (проверка выше),
    // поэтому это вагоны, которые держит именно партия, а не инфраструктура.
    let mut mb_middle_cars = 0_i32; // достижим порог middle (3) → смягчение middle поможет
    let mut mb_mass_cars = 0_i32;   // минимальный порог — массовая выгрузка (5)
    let mut mb_route_cars = 0_i32;  // минимальный порог — маршрутная отправка (10)

    for &d_idx in &unmet_nodes {
        let d = &demand[d_idx];
        let rem = rem_demand[d_idx];

        let node_arcs = arcs_by_demand
            .get(&d_idx)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        // Разбиваем входящие дуги по статусу источника предложения.
        let mut feasible: Vec<usize> = Vec::new();          // источник свободен, не заблок.
        let mut min_batch_blocked: Vec<usize> = Vec::new();
        let mut dmzi_blocked: Vec<usize> = Vec::new();
        let mut source_exhausted = 0_usize;

        let mut min_arc_cost = f64::INFINITY;
        let mut min_batch_pairs: HashMap<PairKey, (i32, i32)> = HashMap::new();
        let mut dmzi_buckets: BTreeSet<usize> = BTreeSet::new();

        for &arc_id in node_arcs {
            let arc = &arcs[arc_id];
            let s_rem = rem_supply[arc.s_idx];

            if s_rem <= 0 {
                source_exhausted += 1;
                continue;
            }

            // Квота ДМЗИ — жёсткий потолок дороги, проверяется ПЕРВОЙ. Иначе дуга,
            // одновременно «ниже партии» и на исчерпанной дороге, ложно
            // приписывается MIN_BATCH, хотя реально её держит инфраструктура.
            if let Some((idx, used)) = &dmzi {
                if let Some(b) = idx.arc_bucket[arc_id] {
                    if used[b] >= idx.buckets[b].1 {
                        dmzi_blocked.push(arc_id);
                        dmzi_buckets.insert(b);
                        continue;
                    }
                }
            }

            // Ограничение минимальной партии (на дороге ещё есть свободная квота ДМЗИ,
            // значит держит именно партия, а не инфраструктура).
            if arc.has_pair_min_batch() {
                let b = arc.pair_min_batch;
                let key = arc.pair_key();
                let flow = pair_flow.get(&key).copied().unwrap_or(0);
                let add_potential = s_rem.min(rem);
                let blocked = if flow == 0 {
                    add_potential < b
                } else {
                    flow < b
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

            feasible.push(arc_id);
            if arc.cost < min_arc_cost { min_arc_cost = arc.cost; }
        }

        let cause = if node_arcs.is_empty() {
            // Структурный разбор: почему пара с каждым узлом предложения отброшена.
            let (mut no_tariff, mut bad_type, mut dirty_etsng, mut bad_period) = (0, 0, 0, 0);
            for s in supply.iter() {
                let s_wash_min = wash_min_cost.get(s.station_to_code.as_str()).copied();
                match classify_pair(s, d, &tariff_index, wash_codes, no_cleaning_roads, washed_empty_codes, wash_tariffs, s_wash_min) {
                    // Feasible здесь невозможен: иначе дуга была бы построена.
                    PairOutcome::Feasible { .. } => {}
                    PairOutcome::NoTariff => no_tariff += 1,
                    PairOutcome::BadType => bad_type += 1,
                    // Грязный вагон: и несовпадение ЕТСНГ, и «дальняя погрузка дороже промывки»
                    // объединяем в один счётчик «грязный не может ехать под эту погрузку».
                    PairOutcome::DirtyEtsngMismatch => dirty_etsng += 1,
                    PairOutcome::DirtyFarLoadPreferWash => dirty_etsng += 1,
                    PairOutcome::BadPeriod => bad_period += 1,
                }
            }
            UnmetCause::NoFeasibleArcs {
                supply_nodes_total: supply.len(),
                no_tariff, bad_type, dirty_etsng, bad_period,
            }
        } else if feasible.is_empty() && !min_batch_blocked.is_empty() {
            // Класс по самому низкому достижимому порогу: его смягчение помогло бы.
            let min_b = min_batch_pairs.keys().map(|(_, _, b)| *b).min().unwrap_or(0);
            if min_b == MIN_BATCH_TO_ROUTE_DEMAND_STATION {
                mb_route_cars += rem;
            } else if min_b == MIN_BATCH_FROM_MASS_STATION {
                mb_mass_cars += rem;
            } else {
                mb_middle_cars += rem;
            }
            UnmetCause::MinBatchDeadlock {
                pairs: min_batch_pairs
                    .into_iter()
                    .map(|((ss, ds, b), (f, p))| (ss, ds, f, p, b))
                    .collect(),
            }
        } else if feasible.is_empty() && !dmzi_blocked.is_empty() {
            let (idx, used) = dmzi.as_ref().expect("dmzi_buckets непусто только при Some");
            UnmetCause::DmziQuotaExhausted {
                buckets: dmzi_buckets
                    .iter()
                    .map(|&b| {
                        let ((rw, period), limit) = &idx.buckets[b];
                        (rw.clone(), *period, used[b], *limit)
                    })
                    .collect(),
            }
        } else if feasible.is_empty() {
            UnmetCause::AllSourcesExhausted { arc_count: source_exhausted }
        } else if min_arc_cost >= PENALTY_UNMET {
            UnmetCause::PenaltyCheaperThanArcs {
                feasible_arcs_count: feasible.len(),
                min_arc_cost_per_wagon: min_arc_cost,
            }
        } else {
            let mut top: Vec<(String, f64, i32)> = feasible
                .iter()
                .map(|&aid| {
                    let arc = &arcs[aid];
                    (supply[arc.s_idx].station_to.clone(), arc.cost, rem_supply[arc.s_idx])
                })
                .collect();
            top.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            top.truncate(3);
            UnmetCause::UnexpectedNotUsed {
                feasible_arcs_count: feasible.len(),
                min_arc_cost_per_wagon: min_arc_cost,
                top_sources: top,
            }
        };

        println!(
            "  [d_idx {:>4}] {} ({}) | тип={} | ЕТСНГ={} | период={} | отправка={} | не закрыто {} из {} ваг.",
            d_idx,
            d.station_name,
            d.railway_name,
            d.car_type.as_deref().unwrap_or("—"),
            d.etsng.as_deref().unwrap_or("—"),
            d.period,
            d.shipping_type.as_deref().unwrap_or("—"),
            rem,
            d.car_count,
        );
        match &cause {
            UnmetCause::NoFeasibleArcs { supply_nodes_total, no_tariff, bad_type, dirty_etsng, bad_period } => {
                println!(
                    "    ПРИЧИНА: нет ни одной допустимой дуги — закрыть невозможно текущими данными. Отбраковка пар со всеми {} узлами предложения:",
                    supply_nodes_total,
                );
                println!(
                    "             нет тарифа {}, несовм. тип {}, грязный ЕТСНГ {}, нарушение срока {}.",
                    no_tariff, bad_type, dirty_etsng, bad_period,
                );
                add_stat("no_arcs", rem, &mut cause_stats);
            }
            UnmetCause::AllSourcesExhausted { arc_count } => {
                println!(
                    "    ПРИЧИНА: дуги есть ({} шт.), но все совместимые узлы предложения исчерпаны — вагоны ушли на другие заявки (конкуренция за предложение).",
                    arc_count,
                );
                add_stat("sources_exhausted", rem, &mut cause_stats);
            }
            UnmetCause::MinBatchDeadlock { pairs } => {
                println!(
                    "    ПРИЧИНА: MIN_BATCH-тупик ({} пар). Все дуги со свободным предложением — пары с потоком ниже порога партии:",
                    pairs.len(),
                );
                for (ss, ds, flow, potential, min_batch) in pairs.iter().take(5) {
                    println!(
                        "      · ({ss} → {ds}): текущий поток {flow} ваг., макс. добавим {potential} (порог {min_batch})",
                    );
                }
                if pairs.len() > 5 {
                    println!("      · ...ещё {} пар", pairs.len() - 5);
                }
                add_stat("min_batch_deadlock", rem, &mut cause_stats);
            }
            UnmetCause::DmziQuotaExhausted { buckets } => {
                println!(
                    "    ПРИЧИНА: квота ДМЗИ исчерпана — все дуги со свободным предложением идут на дороги с выбранным лимитом:"
                );
                for (rw, period, used, limit) in buckets.iter().take(5) {
                    println!(
                        "      · дорога {rw}, период {period}: использовано {used} из {limit} ваг.",
                    );
                }
                if buckets.len() > 5 {
                    println!("      · ...ещё {} бакетов", buckets.len() - 5);
                }
                add_stat("dmzi_quota", rem, &mut cause_stats);
            }
            UnmetCause::PenaltyCheaperThanArcs { feasible_arcs_count, min_arc_cost_per_wagon } => {
                println!(
                    "    ПРИЧИНА: дуги со свободным предложением есть ({} шт.), но мин. стоимость {:.0} руб./ваг. ≥ PENALTY_UNMET ({:.0}). Штраф unmet дешевле маршрута.",
                    feasible_arcs_count, min_arc_cost_per_wagon, PENALTY_UNMET,
                );
                add_stat("penalty_cheaper", rem, &mut cause_stats);
            }
            UnmetCause::UnexpectedNotUsed { feasible_arcs_count, min_arc_cost_per_wagon, top_sources } => {
                println!(
                    "    ПРИЧИНА: дуги со свободным предложением есть ({} шт., мин. стоимость {:.0} руб./ваг. < PENALTY_UNMET {:.0}), но не задействованы.",
                    feasible_arcs_count, min_arc_cost_per_wagon, PENALTY_UNMET,
                );
                println!("             Вероятно, каскадный эффект MIN_BATCH на соседних парах. ТОП-3 самых дешёвых источника:");
                for (ss, cost, s_rem) in top_sources {
                    println!("      · ← {ss}: cost={:.0} руб./ваг., rem_supply={}", cost, s_rem);
                }
                add_stat("unexpected", rem, &mut cause_stats);
            }
        }
    }

    // Сводка.
    println!();
    println!("  СВОДКА ПО ПРИЧИНАМ:");
    let cars_for = |key: &str| cause_stats.get(key).map(|(_, c)| *c).unwrap_or(0);
    for (cause, (n_nodes, n_cars)) in &cause_stats {
        let label = match *cause {
            "no_arcs"            => "нет допустимых дуг (структурно)",
            "sources_exhausted"  => "источники предложения исчерпаны",
            "min_batch_deadlock" => "MIN_BATCH-тупик (квота ДМЗИ ещё есть)",
            "dmzi_quota"         => "квота ДМЗИ исчерпана (инфраструктура)",
            "penalty_cheaper"    => "штраф unmet < стоимости дуг",
            "unexpected"         => "дуги есть, но не использованы",
            _                    => cause,
        };
        println!("    {:38} узлов: {:>3}, вагонов: {:>4}", label, n_nodes, n_cars);
    }

    // Разбивка MIN_BATCH-тупика по классу самого низкого порога: показывает, какому
    // классу станций смягчение партии реально помогло бы (на этих дорогах квота ДМЗИ
    // ещё не исчерпана, иначе узел попал бы в категорию ДМЗИ).
    let mb_total = cars_for("min_batch_deadlock");
    if mb_total > 0 {
        println!(
            "      из них держит партия по классам: middle (порог {}) {} ваг.; массовая (порог {}) {} ваг.; маршрутная (порог {}) {} ваг.",
            MIN_BATCH_TO_MIDDLE_DEMAND_STATION, mb_middle_cars,
            MIN_BATCH_FROM_MASS_STATION, mb_mass_cars,
            MIN_BATCH_TO_ROUTE_DEMAND_STATION, mb_route_cars,
        );
    }

    let unreachable = cars_for("no_arcs");
    let dmzi_capped = cars_for("dmzi_quota");
    let other = cars_for("sources_exhausted") + cars_for("penalty_cheaper") + cars_for("unexpected");

    println!();
    println!("  ЗАКРЫВАЕМОСТЬ (по реальному связывающему ограничению):");
    println!(
        "    структурно недостижимо (нет дуг):          {:>4} ваг.  — нужны новые тарифы / смягчение жёстких фильтров",
        unreachable,
    );
    println!(
        "    упёрто в квоту ДМЗИ (инфраструктура):       {:>4} ваг.  — поможет только повышение квоты на дороге",
        dmzi_capped,
    );
    println!(
        "    держит MIN_BATCH (квота ДМЗИ ещё есть):     {:>4} ваг.  — поможет смягчение партии (middle {} / масс. {} / маршр. {})",
        mb_total, mb_middle_cars, mb_mass_cars, mb_route_cars,
    );
    println!(
        "    прочее (конкуренция / экономика штрафа):    {:>4} ваг.",
        other,
    );
    println!("--------------------------------");
}

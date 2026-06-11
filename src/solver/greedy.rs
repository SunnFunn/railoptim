use std::collections::{HashMap, HashSet, VecDeque};

use crate::node::{DemandNode, DemandPurpose, SupplyNode};
use super::model::{DmziIndex, DmziLimits, PairKey, TaskArc};

// ---------------------------------------------------------------------------
// Квоты ДМЗИ внутри жадного алгоритма
// ---------------------------------------------------------------------------

/// Остаток квоты бакета ДМЗИ для дуги `arc_pos` (позиция в `arcs`).
/// Дуги вне квот не ограничены.
#[inline]
fn dmzi_rem(arc_bucket: &[Option<usize>], quota_rem: &[i32], arc_pos: usize) -> i32 {
    match arc_bucket.get(arc_pos).copied().flatten() {
        Some(b) => quota_rem[b],
        None => i32::MAX,
    }
}

/// Списывает `qty` вагонов с квоты бакета дуги `arc_pos` (если дуга под квотой).
#[inline]
fn dmzi_consume(arc_bucket: &[Option<usize>], quota_rem: &mut [i32], arc_pos: usize, qty: i32) {
    if let Some(b) = arc_bucket.get(arc_pos).copied().flatten() {
        quota_rem[b] -= qty;
    }
}

// ---------------------------------------------------------------------------
// Результат жадного решения
// ---------------------------------------------------------------------------

/// Назначение одного узла предложения на один узел спроса.
#[derive(Debug, Clone)]
pub struct Assignment {
    /// Индекс дуги в плоском списке `arcs`.
    pub arc_id: usize,
    /// Индекс узла предложения.
    pub s_idx: usize,
    /// Индекс узла спроса.
    pub d_idx: usize,
    /// Количество назначенных вагонов.
    pub quantity: i32,
    /// Стоимость назначения (quantity * arc.cost).
    pub total_cost: f64,
}

/// Сводка жадного решения.
#[derive(Debug, Clone)]
pub struct GreedyResult {
    /// Список конкретных назначений.
    pub assignments: Vec<Assignment>,
    /// Суммарная стоимость по реальным дугам.
    pub total_cost: f64,
    /// Вагоны, успешно назначенные на реальные узлы спроса.
    pub assigned_cars: i32,
    /// Неудовлетворённый спрос (нет дуг или иссякло предложение).
    pub unmet_demand: i32,
    /// Незадействованное предложение (нет подходящих узлов спроса).
    pub excess_supply: i32,
}

// ---------------------------------------------------------------------------
// Остаточная сеть и поток (Edmonds–Karp), ограничение сверху
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ResEdge {
    to: usize,
    rev: usize,
    cap: i32,
}

fn add_residual_edge(g: &mut Vec<Vec<ResEdge>>, fr: usize, to: usize, cap: i32) {
    let rev_to = g[to].len();
    let rev_fr = g[fr].len();
    g[fr].push(ResEdge {
        to,
        rev: rev_to,
        cap,
    });
    g[to].push(ResEdge {
        to: fr,
        rev: rev_fr,
        cap: 0,
    });
}

/// Ориентированное ребро слева направо: позиция в `g[fr]` после добавления.
struct TrackedForward {
    fr: usize,
    pos: usize,
    arc_idx: usize,
    cap0: i32,
}

/// Максимальный поток из `s` в `t`, не превосходящий `limit`.
/// Возвращает величину отправленного потока.
fn max_flow_edmonds_karp_limit(
    g: &mut Vec<Vec<ResEdge>>,
    s: usize,
    t: usize,
    limit: i32,
) -> i32 {
    let n = g.len();
    let mut flow = 0;
    while flow < limit {
        let mut parent: Vec<Option<(usize, usize)>> = vec![None; n]; // (вершина, индекс ребра из неё)
        let mut q = VecDeque::new();
        parent[s] = Some((s, 0));
        q.push_back(s);
        while let Some(v) = q.pop_front() {
            if v == t {
                break;
            }
            for (ei, e) in g[v].iter().enumerate() {
                if e.cap <= 0 {
                    continue;
                }
                if parent[e.to].is_none() {
                    parent[e.to] = Some((v, ei));
                    q.push_back(e.to);
                }
            }
        }
        if parent[t].is_none() {
            break;
        }

        let mut add = limit - flow;
        let mut cur = t;
        while cur != s {
            let (pv, ei) = parent[cur].unwrap();
            add = add.min(g[pv][ei].cap);
            cur = pv;
        }

        cur = t;
        while cur != s {
            let (pv, ei) = parent[cur].unwrap();
            let e_to = g[pv][ei].to;
            let e_rev = g[pv][ei].rev;
            g[pv][ei].cap -= add;
            g[e_to][e_rev].cap += add;
            cur = pv;
        }
        flow += add;
    }
    flow
}

/// Жадно набирает по дугам пары (порядок `pair_arc_indices`) до суммарного потока `min_target`.
/// Изменяет `rem_s`, `rem_d`, `quota_rem` (остатки квот ДМЗИ).
/// Возвращает список ненулевых отгрузок по индексам дуг в `arcs`.
fn greedy_fill_mass_pair_to_min(
    pair_arc_indices: &[usize],
    arcs: &[TaskArc],
    rem_s: &mut [i32],
    rem_d: &mut [i32],
    arc_bucket: &[Option<usize>],
    quota_rem: &mut [i32],
    min_target: i32,
) -> Option<Vec<(usize, i32)>> {
    let mut flows: Vec<(usize, i32)> = Vec::new();
    let mut total_pair = 0;

    while total_pair < min_target {
        let mut progressed = false;
        for &arc_idx in pair_arc_indices {
            let arc = &arcs[arc_idx];
            let q = rem_s[arc.s_idx]
                .min(rem_d[arc.d_idx])
                .min(min_target - total_pair)
                .min(dmzi_rem(arc_bucket, quota_rem, arc_idx));
            if q <= 0 {
                continue;
            }
            progressed = true;
            rem_s[arc.s_idx] -= q;
            rem_d[arc.d_idx] -= q;
            dmzi_consume(arc_bucket, quota_rem, arc_idx, q);
            total_pair += q;
            flows.push((arc_idx, q));
            if total_pair >= min_target {
                break;
            }
        }
        if total_pair >= min_target {
            break;
        }
        if !progressed {
            return None;
        }
    }

    Some(merge_flows_by_arc(flows))
}

/// Поток по подграфу пары «массовая выгрузка → погрузка» не более `limit`.
/// Возвращает объёмы по индексам дуг в `arcs`, если достигнут поток `limit`; иначе `None`.
fn dinic_like_mass_pair_flow(
    pair_arc_indices: &[usize],
    arcs: &[TaskArc],
    rem_s: &mut [i32],
    rem_d: &mut [i32],
    limit: i32,
) -> Option<Vec<(usize, i32)>> {
    let mut s_idx_set: Vec<usize> = pair_arc_indices
        .iter()
        .map(|&i| arcs[i].s_idx)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let mut d_idx_set: Vec<usize> = pair_arc_indices
        .iter()
        .map(|&i| arcs[i].d_idx)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    s_idx_set.sort_unstable();
    d_idx_set.sort_unstable();

    let ns = s_idx_set.len();
    let nd = d_idx_set.len();
    if ns == 0 || nd == 0 {
        return None;
    }

    let mut map_s: HashMap<usize, usize> = HashMap::with_capacity(ns);
    for (i, &sidx) in s_idx_set.iter().enumerate() {
        map_s.insert(sidx, i);
    }
    let mut map_d: HashMap<usize, usize> = HashMap::with_capacity(nd);
    for (j, &didx) in d_idx_set.iter().enumerate() {
        map_d.insert(didx, j);
    }

    let src = 0;
    let snk = 1 + ns + nd;
    let n_nodes = snk + 1;
    let mut g: Vec<Vec<ResEdge>> = vec![Vec::new(); n_nodes];

    for (i, &sidx) in s_idx_set.iter().enumerate() {
        let cap = rem_s[sidx];
        if cap > 0 {
            add_residual_edge(&mut g, src, 1 + i, cap);
        }
    }
    for (j, &didx) in d_idx_set.iter().enumerate() {
        let cap = rem_d[didx];
        if cap > 0 {
            add_residual_edge(&mut g, 1 + ns + j, snk, cap);
        }
    }

    let sum_sup: i32 = s_idx_set.iter().map(|&si| rem_s[si]).sum();
    let sum_dem: i32 = d_idx_set.iter().map(|&di| rem_d[di]).sum();
    let inf = sum_sup.max(sum_dem).max(limit);

    let mut tracked: Vec<TrackedForward> = Vec::with_capacity(pair_arc_indices.len());
    for &arc_idx in pair_arc_indices {
        let arc = &arcs[arc_idx];
        let Some(&si) = map_s.get(&arc.s_idx) else {
            continue;
        };
        let Some(&dj) = map_d.get(&arc.d_idx) else {
            continue;
        };
        let fr = 1 + si;
        let to = 1 + ns + dj;
        let pos = g[fr].len();
        add_residual_edge(&mut g, fr, to, inf);
        tracked.push(TrackedForward {
            fr,
            pos,
            arc_idx,
            cap0: inf,
        });
    }

    let sent_total = max_flow_edmonds_karp_limit(&mut g, src, snk, limit);
    if sent_total < limit {
        return None;
    }

    let mut raw: Vec<(usize, i32)> = Vec::new();
    for tr in tracked {
        let residual = g[tr.fr][tr.pos].cap;
        let f = tr.cap0.saturating_sub(residual);
        if f > 0 {
            raw.push((tr.arc_idx, f));
        }
    }

    // Применяем к локальным остаткам для согласованности вызывающего кода
    for &(arc_idx, q) in &raw {
        let arc = &arcs[arc_idx];
        rem_s[arc.s_idx] -= q;
        rem_d[arc.d_idx] -= q;
    }

    Some(merge_flows_by_arc(raw))
}

fn merge_flows_by_arc(mut flows: Vec<(usize, i32)>) -> Vec<(usize, i32)> {
    flows.sort_unstable_by_key(|x| x.0);
    let mut out: Vec<(usize, i32)> = Vec::new();
    for (aid, q) in flows {
        if q == 0 {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if last.0 == aid {
                last.1 += q;
                continue;
            }
        }
        out.push((aid, q));
    }
    out
}

/// Записывает назначения и счётчики по уже применённому к остаткам потоку активации пары
/// (`trial_s` / `trial_d` скопированы в `remaining_*` до вызова).
fn record_assignments_for_mass_pair_flows(
    flows: &[(usize, i32)],
    arcs: &[TaskArc],
    mass_pair_totals: &mut HashMap<PairKey, i32>,
    key: &PairKey,
    assignments: &mut Vec<Assignment>,
    total_cost: &mut f64,
    assigned_cars: &mut i32,
) {
    let mut add_pair = 0;
    for &(arc_idx, qty) in flows {
        if qty <= 0 {
            continue;
        }
        let arc = &arcs[arc_idx];
        add_pair += qty;
        let arc_cost = qty as f64 * arc.cost;
        *total_cost += arc_cost;
        *assigned_cars += qty;
        assignments.push(Assignment {
            arc_id: arc.arc_id,
            s_idx: arc.s_idx,
            d_idx: arc.d_idx,
            quantity: qty,
            total_cost: arc_cost,
        });
    }
    *mass_pair_totals.entry(key.clone()).or_insert(0) += add_pair;
}

/// Активация пары с ограничением минимальной партии: поток по паре становится
/// ≥ `min_target` (порог пары из `TaskArc::pair_min_batch`) или пара запрещается.
///
/// Квоты ДМЗИ (`arc_bucket` / `quota_rem`) учитываются в обоих путях активации:
/// жадный набор клиппит каждую отгрузку остатком квоты бакета; поток Edmonds–Karp
/// квот не знает, поэтому его результат проверяется пост-фактум — при превышении
/// квоты пара запрещается (консервативно: партия в обход квоты не собирается).
#[allow(clippy::too_many_arguments)]
fn try_activate_mass_pair(
    key: &PairKey,
    pair_arc_indices: &[usize],
    arcs: &[TaskArc],
    min_target: i32,
    remaining_supply: &mut Vec<i32>,
    remaining_demand: &mut Vec<i32>,
    arc_bucket: &[Option<usize>],
    quota_rem: &mut Vec<i32>,
    mass_pair_totals: &mut HashMap<PairKey, i32>,
    forbidden_pairs: &mut HashSet<PairKey>,
    assignments: &mut Vec<Assignment>,
    total_cost: &mut f64,
    assigned_cars: &mut i32,
) {
    if pair_arc_indices.is_empty() {
        forbidden_pairs.insert(key.clone());
        return;
    }

    let mut trial_s = remaining_supply.clone();
    let mut trial_d = remaining_demand.clone();
    let mut trial_q = quota_rem.clone();

    if let Some(flows) = greedy_fill_mass_pair_to_min(
        pair_arc_indices,
        arcs,
        &mut trial_s,
        &mut trial_d,
        arc_bucket,
        &mut trial_q,
        min_target,
    ) {
        remaining_supply.clone_from(&trial_s);
        remaining_demand.clone_from(&trial_d);
        quota_rem.clone_from(&trial_q);
        record_assignments_for_mass_pair_flows(
            &flows,
            arcs,
            mass_pair_totals,
            key,
            assignments,
            total_cost,
            assigned_cars,
        );
        return;
    }

    let mut trial_s = remaining_supply.clone();
    let mut trial_d = remaining_demand.clone();
    if let Some(flows) = dinic_like_mass_pair_flow(
        pair_arc_indices,
        arcs,
        &mut trial_s,
        &mut trial_d,
        min_target,
    ) {
        // Пост-проверка квот ДМЗИ: поток Edmonds–Karp строится без учёта квот.
        let mut trial_q = quota_rem.clone();
        let mut quota_ok = true;
        for &(arc_idx, q) in &flows {
            if let Some(b) = arc_bucket.get(arc_idx).copied().flatten() {
                trial_q[b] -= q;
                if trial_q[b] < 0 {
                    quota_ok = false;
                    break;
                }
            }
        }
        if quota_ok {
            remaining_supply.clone_from(&trial_s);
            remaining_demand.clone_from(&trial_d);
            quota_rem.clone_from(&trial_q);
            record_assignments_for_mass_pair_flows(
                &flows,
                arcs,
                mass_pair_totals,
                key,
                assignments,
                total_cost,
                assigned_cars,
            );
            return;
        }
    }

    forbidden_pairs.insert(key.clone());
}

// ---------------------------------------------------------------------------
// Жадный алгоритм
// ---------------------------------------------------------------------------

/// Строит начальное допустимое решение жадным методом.
///
/// # Стратегия
///
/// 1. Отбрасываем дуги с `car_type_ok == false`.
/// 2. Сортируем допустимые дуги по стоимости, затем по расстоянию.
/// 3. Для дуг **без** ограничения партии (`pair_min_batch == 0`) — классическое
///    назначение `min(остаток_s, остаток_d)`.
/// 4. Для дуг с `pair_min_batch > 0` (массовая выгрузка, средние станции) ограничение
///    минимальной партии на пару станций `(образование порожнего → погрузка)`:
///    - индекс всех допустимых дуг по ключу пары станций;
///    - при первом обращении к паре: набрать не менее `pair_min_batch` суммарного потока
///      (сначала жадно в порядке дуг пары по стоимости; если не удалось — поток в двудольном
///      подграфе пары, Edmonds–Karp с лимитом `pair_min_batch`);
///    - если достичь порога невозможно, пара помечается запрещённой (нулевой поток);
///    - при уже активированной паре — обычное добавление по текущей дуге.
/// 5. Квоты ДМЗИ (`dmzi_limits`): каждое назначение на Load-узел клиппится
///    остатком квоты бакета `(дорога погрузки, период предложения)`; активация
///    MIN_BATCH-пары не может собрать партию в обход квоты.
///
/// Жадность по стоимости глобально сохраняется порядком обхода отсортированных дуг;
/// внутри пары при активации дуги упорядочены по `(cost, distance)`.
pub fn greedy_initial_solution(
    arcs: &[TaskArc],
    supply: &[SupplyNode],
    demand: &[DemandNode],
    dmzi_limits: Option<&DmziLimits>,
) -> GreedyResult {
    let mut remaining_supply: Vec<i32> = supply.iter().map(|s| s.car_count).collect();
    let mut remaining_demand: Vec<i32> = demand.iter().map(|d| d.car_count).collect();

    // Квоты ДМЗИ: arc_bucket — позиция дуги → бакет; quota_rem — остатки лимитов.
    let dmzi_index = dmzi_limits
        .filter(|l| !l.is_empty())
        .map(|l| DmziIndex::build(arcs, supply, demand, l));
    let (arc_bucket, mut quota_rem): (Vec<Option<usize>>, Vec<i32>) = match &dmzi_index {
        Some(idx) => (idx.arc_bucket.clone(), idx.limits_vec()),
        None => (vec![None; arcs.len()], vec![]),
    };

    let mut feasible_arc_indices: Vec<usize> = arcs
        .iter()
        .enumerate()
        .filter(|(_, arc)| arc.car_type_ok)
        .map(|(i, _)| i)
        .collect();

    feasible_arc_indices.sort_unstable_by(|&a, &b| {
        let arc_a = &arcs[a];
        let arc_b = &arcs[b];
        arc_a
            .cost
            .partial_cmp(&arc_b.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| arc_a.distance.cmp(&arc_b.distance))
    });

    // Группы дуг с ограничением минимальной партии: только допустимые по типу дуги.
    let mut mass_pair_arc_indices: HashMap<PairKey, Vec<usize>> = HashMap::new();
    for (i, arc) in arcs.iter().enumerate() {
        if arc.has_pair_min_batch() && arc.car_type_ok {
            mass_pair_arc_indices.entry(arc.pair_key()).or_default().push(i);
        }
    }
    for v in mass_pair_arc_indices.values_mut() {
        v.sort_unstable_by(|&a, &b| {
            let arca = &arcs[a];
            let arcb = &arcs[b];
            arca
                .cost
                .partial_cmp(&arcb.cost)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| arca.distance.cmp(&arcb.distance))
        });
    }

    let mut assignments: Vec<Assignment> = Vec::new();
    let mut total_cost: f64 = 0.0;
    let mut assigned_cars: i32 = 0;

    let mut mass_pair_totals: HashMap<PairKey, i32> = HashMap::new();
    let mut forbidden_pairs: HashSet<PairKey> = HashSet::new();

    for arc_i in feasible_arc_indices {
        let arc = &arcs[arc_i];

        let avail_supply = remaining_supply[arc.s_idx];
        let avail_demand = remaining_demand[arc.d_idx];

        if avail_supply <= 0 || avail_demand <= 0 {
            continue;
        }

        if arc.has_pair_min_batch() {
            let key = arc.pair_key();
            if forbidden_pairs.contains(&key) {
                continue;
            }

            let pair_flow = mass_pair_totals.get(&key).copied().unwrap_or(0);
            if pair_flow == 0 {
                let Some(pair_indices) = mass_pair_arc_indices.get(&key) else {
                    forbidden_pairs.insert(key.clone());
                    continue;
                };
                try_activate_mass_pair(
                    &key,
                    pair_indices,
                    arcs,
                    arc.pair_min_batch,
                    &mut remaining_supply,
                    &mut remaining_demand,
                    &arc_bucket,
                    &mut quota_rem,
                    &mut mass_pair_totals,
                    &mut forbidden_pairs,
                    &mut assignments,
                    &mut total_cost,
                    &mut assigned_cars,
                );
                if forbidden_pairs.contains(&key) {
                    continue;
                }
            }

            let avail_supply = remaining_supply[arc.s_idx];
            let avail_demand = remaining_demand[arc.d_idx];
            if avail_supply <= 0 || avail_demand <= 0 {
                if demand_load_exhausted(demand, &remaining_demand) {
                    break;
                }
                continue;
            }

            let qty = avail_supply
                .min(avail_demand)
                .min(dmzi_rem(&arc_bucket, &quota_rem, arc_i));
            if qty <= 0 {
                continue;
            }
            remaining_supply[arc.s_idx] -= qty;
            remaining_demand[arc.d_idx] -= qty;
            dmzi_consume(&arc_bucket, &mut quota_rem, arc_i, qty);

            let arc_cost = qty as f64 * arc.cost;
            total_cost += arc_cost;
            assigned_cars += qty;

            assignments.push(Assignment {
                arc_id: arc.arc_id,
                s_idx: arc.s_idx,
                d_idx: arc.d_idx,
                quantity: qty,
                total_cost: arc_cost,
            });

            *mass_pair_totals.entry(key).or_insert(0) += qty;
        } else {
            let qty = avail_supply
                .min(avail_demand)
                .min(dmzi_rem(&arc_bucket, &quota_rem, arc_i));
            if qty <= 0 {
                continue;
            }

            remaining_supply[arc.s_idx] -= qty;
            remaining_demand[arc.d_idx] -= qty;
            dmzi_consume(&arc_bucket, &mut quota_rem, arc_i, qty);

            let arc_cost = qty as f64 * arc.cost;
            total_cost += arc_cost;
            assigned_cars += qty;

            assignments.push(Assignment {
                arc_id: arc.arc_id,
                s_idx: arc.s_idx,
                d_idx: arc.d_idx,
                quantity: qty,
                total_cost: arc_cost,
            });
        }

        if demand_load_exhausted(demand, &remaining_demand) {
            break;
        }
    }

    let unmet_demand: i32 = remaining_demand
        .iter()
        .zip(demand.iter())
        .filter(|(r, d)| d.purpose == DemandPurpose::Load && **r > 0)
        .map(|(r, _)| *r)
        .sum();
    let excess_supply: i32 = remaining_supply.iter().filter(|&&s| s > 0).sum();

    GreedyResult {
        assignments,
        total_cost,
        assigned_cars,
        unmet_demand,
        excess_supply,
    }
}

fn demand_load_exhausted(demand: &[DemandNode], remaining_demand: &[i32]) -> bool {
    demand
        .iter()
        .zip(remaining_demand.iter())
        .all(|(d, &r)| d.purpose != DemandPurpose::Load || r <= 0)
}

// ---------------------------------------------------------------------------
// Конвертация жадного решения в формат LP (Vec<f64> по arc_id)
// ---------------------------------------------------------------------------

/// Переводит `GreedyResult` в плоский вектор значений переменных LP,
/// совместимый с форматом `arc_vals` из `solve()`.
///
/// Индекс в векторе = `arc.arc_id`. Значение = сумма назначенных вагонов по дуге.
/// Дуги без назначения получают 0.0.
pub fn greedy_to_arc_vals(result: &GreedyResult, n_arcs: usize) -> Vec<f64> {
    let mut arc_vals = vec![0.0_f64; n_arcs];
    for assignment in &result.assignments {
        arc_vals[assignment.arc_id] += assignment.quantity as f64;
    }
    arc_vals
}

// ---------------------------------------------------------------------------
// Диагностика
// ---------------------------------------------------------------------------

/// Выводит сводку жадного решения в консоль.
pub fn print_greedy_result(result: &GreedyResult, supply: &[SupplyNode], demand: &[DemandNode]) {
    let total_supply: i32 = supply.iter().map(|s| s.car_count).sum();
    let total_load_demand: i32 = demand
        .iter()
        .filter(|d| d.purpose == DemandPurpose::Load)
        .map(|d| d.car_count)
        .sum();

    println!("--- ЖАДНОЕ РЕШЕНИЕ ---");
    println!("Назначений:            {} шт.", result.assignments.len());
    println!(
        "Назначено вагонов:     {} / {} спрос (погрузка), {} предложение",
        result.assigned_cars, total_load_demand, total_supply
    );
    println!("Суммарная стоимость:   {:.2} руб.", result.total_cost);
    println!("Неудовлетворён спрос:  {} ваг.", result.unmet_demand);
    println!("Избыток предложения:   {} ваг.", result.excess_supply);
    if total_load_demand > 0 {
        println!(
            "Покрытие спроса (погр.): {:.1}%",
            result.assigned_cars as f64 / total_load_demand as f64 * 100.0
        );
    }
    println!("----------------------");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::DemandPurpose;
    use crate::solver::model::MIN_BATCH_FROM_MASS_STATION;

    fn dummy_supply(count: i32, station_code: &str, s_idx: usize, mass: bool) -> SupplyNode {
        SupplyNode {
            s_id: s_idx + 1,
            kind: crate::node::CarKind::Free,
            car_count: count,
            station_to: String::new(),
            station_to_code: station_code.to_string(),
            railway_to: String::new(),
            railway_to_code: None,
            railway_part_to: None,
            car_type: Some("Прочие".to_string()),
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
            is_mass_unloading: mass,
            prev_etsngs: vec![],
            prev_etsng_names: vec![],
        }
    }

    fn dummy_demand(count: i32, station_code: &str, d_idx: usize) -> DemandNode {
        DemandNode {
            d_id: d_idx + 1,
            purpose: DemandPurpose::Load,
            period: 1,
            station_name: String::new(),
            station_code: station_code.to_string(),
            railway_name: String::new(),
            railway_code: None,
            railway_part: None,
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
            gng_cargo: None,
            etsng: None,
            request_numbers: None,
            request_dates: None,
            gu12_number: None,
            shipping_type: None,
            car_type: Some("Прочие".to_string()),
            car_count: count,
            cars_on_station: 0,
        }
    }

    fn arc(
        id: usize,
        s: usize,
        d: usize,
        from_st: &str,
        to_st: &str,
        cost: f64,
        mass: bool,
    ) -> TaskArc {
        TaskArc {
            arc_id: id,
            s_idx: s,
            d_idx: d,
            supply_station_code: from_st.to_string(),
            demand_station_code: to_st.to_string(),
            cost,
            distance: 1,
            delivery_days: 1,
            period_ok: true,
            car_type_ok: true,
            pair_min_batch: if mass { MIN_BATCH_FROM_MASS_STATION } else { 0 },
        }
    }

    /// Дуга с явным порогом партии пары (средние станции).
    fn arc_b(
        id: usize,
        s: usize,
        d: usize,
        from_st: &str,
        to_st: &str,
        cost: f64,
        pair_min_batch: i32,
    ) -> TaskArc {
        TaskArc {
            pair_min_batch,
            ..arc(id, s, d, from_st, to_st, cost, false)
        }
    }

    /// Спрос на B меньше порога партии — пара массовой станции не должна открыться (поток 0).
    #[test]
    fn mass_pair_forbidden_when_demand_less_than_min_batch() {
        let b = MIN_BATCH_FROM_MASS_STATION;
        if b <= 1 {
            return; // при пороге ≤ 1 спрос b − 1 вырожден — запрещать нечего
        }
        let supply = vec![
            dummy_supply(b + 2, "A", 0, true),
        ];
        let demand = vec![
            dummy_demand(b - 1, "B", 0),
        ];
        let arcs = vec![
            arc(0, 0, 0, "A", "B", 100.0, true),
        ];
        let r = greedy_initial_solution(&arcs, &supply, &demand, None);
        assert_eq!(r.assignments.len(), 0);
        assert_eq!(r.unmet_demand, b - 1);
    }

    /// Два узла спроса на B, каждый меньше порога — сумма ≥ порога, активация возможна.
    #[test]
    fn mass_pair_activates_when_aggregate_demand_ge_min_batch() {
        let b = MIN_BATCH_FROM_MASS_STATION;
        let half = b / 2 + 1; // каждый узел < b (при b ≥ 3), сумма 2·half ≥ b
        let supply = vec![dummy_supply(2 * half + 1, "A", 0, true)];
        let demand = vec![
            dummy_demand(half, "B", 0),
            dummy_demand(half, "B", 1),
        ];
        let arcs = vec![
            arc(0, 0, 0, "A", "B", 10.0, true),
            arc(1, 0, 1, "A", "B", 20.0, true),
        ];
        let r = greedy_initial_solution(&arcs, &supply, &demand, None);
        assert!(r.assigned_cars >= MIN_BATCH_FROM_MASS_STATION);
        assert!(r.unmet_demand <= 1);
    }

    /// Две станции погрузки: активация первой пары не должна ломать вторую.
    #[test]
    fn mass_pair_two_destinations() {
        let b = MIN_BATCH_FROM_MASS_STATION;
        let supply = vec![dummy_supply(2 * b + 4, "A", 0, true)];
        let demand = vec![
            dummy_demand(b, "B", 0),
            dummy_demand(b, "C", 1),
        ];
        let arcs = vec![
            arc(0, 0, 0, "A", "B", 5.0, true),
            arc(1, 0, 1, "A", "C", 6.0, true),
        ];
        let r = greedy_initial_solution(&arcs, &supply, &demand, None);
        let sum_b: i32 = r
            .assignments
            .iter()
            .filter(|a| a.d_idx == 0)
            .map(|a| a.quantity)
            .sum();
        let sum_c: i32 = r
            .assignments
            .iter()
            .filter(|a| a.d_idx == 1)
            .map(|a| a.quantity)
            .sum();
        assert!(sum_b == 0 || sum_b >= MIN_BATCH_FROM_MASS_STATION);
        assert!(sum_c == 0 || sum_c >= MIN_BATCH_FROM_MASS_STATION);
    }

    /// Средняя пара (pair_min_batch=B): достижимый спрос на станции D + дешёвая
    /// альтернатива на 2 ваг. без ограничения. Поток средней пары — 0 или ≥ B,
    /// спрос закрыт полностью.
    #[test]
    fn middle_pair_flow_zero_or_ge_min_batch() {
        let b = crate::solver::model::MIN_BATCH_TO_MIDDLE_DEMAND_STATION;
        let d_big = b.max(2) + 2; // спрос средней станции заведомо ≥ порога
        let supply = vec![dummy_supply(d_big + 2, "M", 0, false)];
        let demand = vec![
            dummy_demand(d_big, "D", 0), // средне-крупная станция спроса
            dummy_demand(2, "C", 1),     // мелкий спрос без ограничения
        ];
        let arcs = vec![
            arc_b(0, 0, 0, "M", "D", 100.0, b),
            arc_b(1, 0, 1, "M", "C", 50.0, 0),
        ];
        let r = greedy_initial_solution(&arcs, &supply, &demand, None);
        let sum_d: i32 = r
            .assignments
            .iter()
            .filter(|a| a.d_idx == 0)
            .map(|a| a.quantity)
            .sum();
        assert!(sum_d == 0 || sum_d >= b, "поток средней пары {} нарушает порог {}", sum_d, b);
        assert_eq!(r.unmet_demand, 0);
    }

    /// Средняя пара: спрос B − 1 ваг. на станции D — порог недостижим, пара
    /// запрещается (поток 0), вагоны не дробятся по одному.
    #[test]
    fn middle_pair_forbidden_when_demand_below_min_batch() {
        let b = crate::solver::model::MIN_BATCH_TO_MIDDLE_DEMAND_STATION;
        if b <= 1 {
            return; // при пороге ≤ 1 спрос b − 1 вырожден — запрещать нечего
        }
        let supply = vec![dummy_supply(b + 4, "M", 0, false)];
        let demand = vec![dummy_demand(b - 1, "D", 0)];
        let arcs = vec![arc_b(0, 0, 0, "M", "D", 100.0, b)];
        let r = greedy_initial_solution(&arcs, &supply, &demand, None);
        assert_eq!(r.assignments.len(), 0);
        assert_eq!(r.unmet_demand, b - 1);
    }

    /// Маршрутная пара (pair_min_batch=B): спрос B + 2, предложение B + 2 —
    /// поток 0 или ≥ B.
    #[test]
    fn route_pair_flow_zero_or_ge_min_batch() {
        let b = crate::solver::model::MIN_BATCH_TO_ROUTE_DEMAND_STATION;
        let n = b + 2;
        let supply = vec![dummy_supply(n, "M", 0, false)];
        let demand = vec![dummy_demand(n, "D", 0)];
        let arcs = vec![arc_b(0, 0, 0, "M", "D", 100.0, b)];
        let r = greedy_initial_solution(&arcs, &supply, &demand, None);
        let sum_d: i32 = r.assignments.iter().map(|a| a.quantity).sum();
        assert!(sum_d == 0 || sum_d >= b, "поток маршрутной пары {} нарушает порог {}", sum_d, b);
        assert_eq!(r.unmet_demand, 0);
    }

    /// Маршрутная пара: спрос B − 1 < порога B — партия недостижима, поток 0.
    #[test]
    fn route_pair_forbidden_when_demand_below_min_batch() {
        let b = crate::solver::model::MIN_BATCH_TO_ROUTE_DEMAND_STATION;
        if b <= 1 {
            return; // при пороге ≤ 1 спрос b − 1 вырожден — запрещать нечего
        }
        let supply = vec![dummy_supply(b + 2, "M", 0, false)];
        let demand = vec![dummy_demand(b - 1, "D", 0)];
        let arcs = vec![arc_b(0, 0, 0, "M", "D", 100.0, b)];
        let r = greedy_initial_solution(&arcs, &supply, &demand, None);
        assert_eq!(r.assignments.len(), 0);
        assert_eq!(r.unmet_demand, b - 1);
    }

    /// MIP: маршрутная (B_route) и средняя (B_mid) группы на одной станции погрузки.
    /// Предложение B_route + B_mid − 1: оптимум — маршрутная партия B_route,
    /// средней группе остаётся B_mid − 1 < B_mid, поэтому её поток обязан быть 0.
    #[test]
    fn mip_separates_route_and_regular_groups_on_same_station() {
        use std::time::Duration;
        let b_route = crate::solver::model::MIN_BATCH_TO_ROUTE_DEMAND_STATION;
        let b_mid = crate::solver::model::MIN_BATCH_TO_MIDDLE_DEMAND_STATION;
        if b_mid < 1 || b_route <= b_mid {
            return; // тест осмыслен, когда маршрутный порог строже среднего
        }
        let supply = vec![dummy_supply(b_route + b_mid - 1, "M", 0, false)];
        let demand = vec![
            dummy_demand(b_route, "D", 0), // маршрутные узлы станции D
            dummy_demand(b_mid, "D", 1),   // обычные узлы той же станции D
        ];
        let arcs = vec![
            arc_b(0, 0, 0, "M", "D", 100.0, b_route),
            arc_b(1, 0, 1, "M", "D", 100.0, b_mid),
        ];
        let outcome = crate::solver::mip::solve_mip(
            &arcs,
            &supply,
            &demand,
            Duration::from_secs(10),
            None,
            None,
            None,
            None,
        );
        assert!(outcome.has_feasible_solution());
        let flow_route = outcome.arc_vals[0].round() as i32;
        let flow_mid = outcome.arc_vals[1].round() as i32;
        assert!(flow_route == 0 || flow_route >= b_route);
        assert!(flow_mid == 0 || flow_mid >= b_mid);
        // PENALTY_UNMET доминирует: выгоднее закрыть маршрутные B_route, чем обычные B_mid.
        assert_eq!(flow_route, b_route);
        assert_eq!(flow_mid, 0);
    }

    // -----------------------------------------------------------------------
    // Квоты ДМЗИ
    // -----------------------------------------------------------------------

    fn dmzi_limits(entries: &[(&str, u8, i32)]) -> crate::solver::model::DmziLimits {
        entries
            .iter()
            .map(|(rw, p, lim)| ((rw.to_string(), *p), *lim))
            .collect()
    }

    fn with_railway(mut d: DemandNode, railway: &str) -> DemandNode {
        d.railway_name = railway.to_string();
        d
    }

    fn with_period(mut s: SupplyNode, period: u8) -> SupplyNode {
        s.supply_period = period;
        s
    }

    /// Квота ДМЗИ клиппит назначение: предложение 5, спрос 5, лимит дороги 3.
    #[test]
    fn dmzi_quota_clamps_greedy_assignment() {
        let supply = vec![dummy_supply(5, "A", 0, false)];
        let demand = vec![with_railway(dummy_demand(5, "B", 0), "МСК")];
        let arcs = vec![arc(0, 0, 0, "A", "B", 100.0, false)];
        let limits = dmzi_limits(&[("МСК", 1, 3)]);
        let r = greedy_initial_solution(&arcs, &supply, &demand, Some(&limits));
        assert_eq!(r.assigned_cars, 3);
        assert_eq!(r.unmet_demand, 2);
        assert_eq!(r.excess_supply, 2);
    }

    /// Периоды 1 и 10 — раздельные бакеты одной дороги.
    #[test]
    fn dmzi_quota_separate_periods() {
        let supply = vec![
            with_period(dummy_supply(5, "A", 0, false), 1),
            with_period(dummy_supply(5, "C", 1, false), 10),
        ];
        let demand = vec![with_railway(dummy_demand(10, "B", 0), "МСК")];
        let arcs = vec![
            arc(0, 0, 0, "A", "B", 100.0, false),
            arc(1, 1, 0, "C", "B", 100.0, false),
        ];
        let limits = dmzi_limits(&[("МСК", 1, 2), ("МСК", 10, 3)]);
        let r = greedy_initial_solution(&arcs, &supply, &demand, Some(&limits));
        let from_p1: i32 = r.assignments.iter().filter(|a| a.s_idx == 0).map(|a| a.quantity).sum();
        let from_p10: i32 = r.assignments.iter().filter(|a| a.s_idx == 1).map(|a| a.quantity).sum();
        assert_eq!(from_p1, 2);
        assert_eq!(from_p10, 3);
        assert_eq!(r.assigned_cars, 5);
    }

    /// Квота B − 1 < порога партии B — MIN_BATCH-пара не активируется (поток 0),
    /// партия не собирается «в обход» квоты ДМЗИ.
    #[test]
    fn dmzi_quota_blocks_min_batch_pair_activation() {
        let b = crate::solver::model::MIN_BATCH_TO_MIDDLE_DEMAND_STATION;
        if b <= 1 {
            return; // при пороге ≤ 1 квота b − 1 = 0 вырожденна
        }
        let supply = vec![dummy_supply(b + 4, "M", 0, false)];
        let demand = vec![with_railway(dummy_demand(b + 2, "D", 0), "МСК")];
        let arcs = vec![arc_b(0, 0, 0, "M", "D", 100.0, b)];
        let limits = dmzi_limits(&[("МСК", 1, b - 1)]);
        let r = greedy_initial_solution(&arcs, &supply, &demand, Some(&limits));
        assert_eq!(r.assignments.len(), 0, "пара должна быть запрещена квотой ДМЗИ");
        assert_eq!(r.unmet_demand, b + 2);
    }

    /// Дорога без лимита в карте квот не ограничивается.
    #[test]
    fn dmzi_quota_ignores_unlisted_railway() {
        let supply = vec![dummy_supply(5, "A", 0, false)];
        let demand = vec![with_railway(dummy_demand(5, "B", 0), "ЮВС")];
        let arcs = vec![arc(0, 0, 0, "A", "B", 100.0, false)];
        let limits = dmzi_limits(&[("МСК", 1, 1)]);
        let r = greedy_initial_solution(&arcs, &supply, &demand, Some(&limits));
        assert_eq!(r.assigned_cars, 5);
    }

    /// MIP: квота ДМЗИ как жёсткая строка модели — поток на дорогу не превышает лимит.
    #[test]
    fn mip_dmzi_quota_enforced() {
        use std::time::Duration;
        let supply = vec![dummy_supply(5, "A", 0, false)];
        let demand = vec![with_railway(dummy_demand(5, "B", 0), "МСК")];
        let arcs = vec![arc(0, 0, 0, "A", "B", 100.0, false)];
        let limits = dmzi_limits(&[("МСК", 1, 3)]);
        let outcome = crate::solver::mip::solve_mip(
            &arcs,
            &supply,
            &demand,
            Duration::from_secs(10),
            None,
            None,
            None,
            Some(&limits),
        );
        assert!(outcome.has_feasible_solution());
        let flow = outcome.arc_vals[0].round() as i32;
        assert_eq!(flow, 3, "поток {} должен быть ограничен квотой 3", flow);
        assert_eq!(outcome.optim.penalty_cars.round() as i32, 2);
    }

    /// MIP: периоды 1 и 10 — раздельные бакеты; суммарно закрывается 5 из 10.
    #[test]
    fn mip_dmzi_quota_separate_periods() {
        use std::time::Duration;
        let supply = vec![
            with_period(dummy_supply(5, "A", 0, false), 1),
            with_period(dummy_supply(5, "C", 1, false), 10),
        ];
        let demand = vec![with_railway(dummy_demand(10, "B", 0), "МСК")];
        let arcs = vec![
            arc(0, 0, 0, "A", "B", 100.0, false),
            arc(1, 1, 0, "C", "B", 100.0, false),
        ];
        let limits = dmzi_limits(&[("МСК", 1, 2), ("МСК", 10, 3)]);
        let outcome = crate::solver::mip::solve_mip(
            &arcs,
            &supply,
            &demand,
            Duration::from_secs(10),
            None,
            None,
            None,
            Some(&limits),
        );
        assert!(outcome.has_feasible_solution());
        assert_eq!(outcome.arc_vals[0].round() as i32, 2);
        assert_eq!(outcome.arc_vals[1].round() as i32, 3);
    }

    /// MIP: warm-start из greedy с квотами не отвергается (оба слоя согласованы).
    #[test]
    fn mip_dmzi_accepts_greedy_warm_start() {
        use std::time::Duration;
        let supply = vec![dummy_supply(5, "A", 0, false)];
        let demand = vec![with_railway(dummy_demand(5, "B", 0), "МСК")];
        let arcs = vec![arc(0, 0, 0, "A", "B", 100.0, false)];
        let limits = dmzi_limits(&[("МСК", 1, 3)]);
        let greedy = greedy_initial_solution(&arcs, &supply, &demand, Some(&limits));
        let warm = greedy_to_arc_vals(&greedy, arcs.len());
        let outcome = crate::solver::mip::solve_mip(
            &arcs,
            &supply,
            &demand,
            Duration::from_secs(10),
            Some(&warm),
            None,
            None,
            Some(&limits),
        );
        assert!(outcome.has_feasible_solution());
        assert_eq!(outcome.arc_vals[0].round() as i32, 3);
    }

    /// MIP: средняя пара со спросом 5 и дешёвой альтернативой на 2 ваг. — поток
    /// пары в решении MIP должен быть 0 или ≥ 3 (big-M по pair_min_batch).
    #[test]
    fn mip_middle_pair_flow_zero_or_ge_min_batch() {
        use std::time::Duration;
        let b = crate::solver::model::MIN_BATCH_TO_MIDDLE_DEMAND_STATION;
        let supply = vec![dummy_supply(7, "M", 0, false)];
        let demand = vec![
            dummy_demand(5, "D", 0),
            dummy_demand(2, "C", 1),
        ];
        let arcs = vec![
            arc_b(0, 0, 0, "M", "D", 100.0, b),
            arc_b(1, 0, 1, "M", "C", 50.0, 0),
        ];
        let outcome = crate::solver::mip::solve_mip(
            &arcs,
            &supply,
            &demand,
            Duration::from_secs(10),
            None,
            None,
            None,
            None,
        );
        assert!(outcome.has_feasible_solution());
        let flow_d = outcome.arc_vals[0].round() as i32;
        assert!(flow_d == 0 || flow_d >= b, "поток средней пары {} нарушает порог {}", flow_d, b);
        // 7 вагонов хватает на оба адресата — MIP закрывает весь спрос.
        assert_eq!(outcome.optim.penalty_cars.round() as i32, 0);
    }
}

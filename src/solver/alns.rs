use std::collections::HashSet;
use std::time::{Duration, Instant};
use rand::prelude::*;

use crate::node::{DemandNode, DemandPurpose, SupplyNode};
use super::model::{collect_mass_pair_violations, MIN_BATCH_FROM_MASS_STATION, TaskArc};
use super::greedy::{Assignment, GreedyResult, greedy_to_arc_vals};
use super::lp::{solve, OptimResult, PENALTY_COST};

// ---------------------------------------------------------------------------
// Константы
// ---------------------------------------------------------------------------

/// Бюджет времени ALNS по умолчанию.
const DEFAULT_TIME_BUDGET: Duration = Duration::from_secs(300); // 5 минут

/// Начальная доля разрушения (K): 20% назначений.
const DESTROY_RATIO_INIT: f64 = 0.20;

/// Минимальная доля разрушения.
const DESTROY_RATIO_MIN: f64 = 0.05;

/// Максимальная доля разрушения.
const DESTROY_RATIO_MAX: f64 = 0.50;

/// Шаг адаптации K вверх (решение не улучшается — расширяем окрестность).
const DESTROY_RATIO_STEP_UP: f64 = 0.02;

/// Шаг адаптации K вниз (нашли улучшение — сужаем окрестность).
const DESTROY_RATIO_STEP_DOWN: f64 = 0.01;

/// Сколько итераций без улучшения до увеличения K.
const STAGNATION_THRESHOLD: usize = 50;

/// Количество соседей при расширении контекста LP-ремонта.
/// Для каждого разрушенного узла берём N ближайших по стоимости дуг.
const NEIGHBOUR_ARCS_PER_NODE: usize = 5;

// ---------------------------------------------------------------------------
// Состояние решения
// ---------------------------------------------------------------------------

/// Текущее состояние решения внутри ALNS.
///
/// Хранит назначения в виде мутабельных остатков предложения/спроса,
/// чтобы операторы разрушения/ремонта не пересчитывали их с нуля.
#[derive(Debug, Clone)]
pub struct AlnsState {
    /// Активные назначения.
    pub assignments: Vec<Assignment>,
    /// Текущая суммарная стоимость.
    pub total_cost: f64,
    /// Остатки предложения по s_idx.
    pub remaining_supply: Vec<i32>,
    /// Остатки спроса по d_idx.
    pub remaining_demand: Vec<i32>,
}

impl AlnsState {
    /// Создаёт состояние из результата жадного алгоритма.
    pub fn from_greedy(
        greedy: &GreedyResult,
        supply: &[SupplyNode],
        demand:  &[DemandNode],
    ) -> Self {
        let remaining_supply = supply.iter().map(|s| s.car_count).collect::<Vec<_>>();
        let remaining_demand = demand.iter().map(|d| d.car_count).collect::<Vec<_>>();

        // Вычитаем уже назначенные вагоны.
        let mut state = AlnsState {
            assignments:     greedy.assignments.clone(),
            total_cost:      greedy.total_cost,
            remaining_supply,
            remaining_demand,
        };
        for a in &greedy.assignments {
            state.remaining_supply[a.s_idx] -= a.quantity;
            state.remaining_demand[a.d_idx] -= a.quantity;
        }
        state
    }

    /// Пересчитывает `total_cost` из списка назначений.
    pub fn recalculate_cost(&mut self) {
        self.total_cost = self.assignments.iter().map(|a| a.total_cost).sum();
    }

    /// Полная целевая функция, согласованная с LP:
    /// стоимость реальных дуг + штраф за незакрытый спрос + штраф за избыток предложения.
    pub fn objective_cost(&self, demand: &[DemandNode]) -> f64 {
        let (unmet_demand, excess_supply) = self.unmet_and_excess(demand);
        self.total_cost + PENALTY_COST * (unmet_demand + excess_supply) as f64
    }

    /// Текущие остатки по спросу и предложению.
    ///
    /// «Незакрытый спрос» — только узлы **погрузки**; ёмкость промывки не штрафуется.
    pub fn unmet_and_excess(&self, demand: &[DemandNode]) -> (i32, i32) {
        let unmet_demand: i32 = self
            .remaining_demand
            .iter()
            .zip(demand.iter())
            .filter(|(r, d)| d.purpose == DemandPurpose::Load && **r > 0)
            .map(|(r, _)| *r)
            .sum();
        let excess_supply: i32 = self.remaining_supply.iter().filter(|&&s| s > 0).sum();
        (unmet_demand, excess_supply)
    }

    /// Штрафная часть целевой функции (без реальной стоимости дуг).
    pub fn penalty_component_cost(&self, demand: &[DemandNode]) -> f64 {
        let (unmet_demand, excess_supply) = self.unmet_and_excess(demand);
        PENALTY_COST * (unmet_demand + excess_supply) as f64
    }
}

// ---------------------------------------------------------------------------
// Параметры ALNS
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AlnsConfig {
    /// Бюджет времени.
    pub time_budget: Duration,
    /// Начальная доля разрушения.
    pub destroy_ratio: f64,
    /// Seed для воспроизводимости (None = случайный).
    pub seed: Option<u64>,
}

impl Default for AlnsConfig {
    fn default() -> Self {
        AlnsConfig {
            time_budget:   DEFAULT_TIME_BUDGET,
            destroy_ratio: DESTROY_RATIO_INIT,
            seed:          None,
        }
    }
}

// ---------------------------------------------------------------------------
// Статистика ALNS
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AlnsStats {
    /// Количество выполненных итераций.
    pub iterations: usize,
    /// Количество итераций с улучшением глобального лучшего.
    pub improvements: usize,
    /// История стоимости лучшего решения (каждые 10 итераций).
    pub cost_history: Vec<f64>,
    /// Затраченное время.
    pub elapsed: Duration,
    /// Финальная доля разрушения.
    pub final_destroy_ratio: f64,
}

// ---------------------------------------------------------------------------
// Результат ALNS
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct AlnsResult {
    /// Лучшее найденное решение.
    pub best_state: AlnsState,
    /// Вектор значений дуговых переменных (совместим с LP arc_vals).
    pub arc_vals: Vec<f64>,
    /// Статистика выполнения.
    pub stats: AlnsStats,
}

// ---------------------------------------------------------------------------
// Оператор разрушения: случайное удаление
// ---------------------------------------------------------------------------

/// Случайно удаляет `k` назначений из текущего состояния.
///
/// Возвращает список удалённых назначений (для передачи оператору ремонта).
fn destroy_random(
    state: &mut AlnsState,
    k:     usize,
    rng:   &mut impl Rng,
) -> Vec<Assignment> {
    let n = state.assignments.len();
    if n == 0 { return vec![]; }

    let k = k.min(n);

    // Выбираем k случайных индексов без повторений.
    let mut indices: Vec<usize> = (0..n).collect();
    indices.partial_shuffle(rng, k);
    let mut to_remove: Vec<usize> = indices[..k].to_vec();
    to_remove.sort_unstable_by(|a, b| b.cmp(a)); // обратный порядок для swap_remove

    let mut removed: Vec<Assignment> = Vec::with_capacity(k);
    for idx in to_remove {
        let a = state.assignments.swap_remove(idx);
        // Возвращаем вагоны в остатки.
        state.remaining_supply[a.s_idx] += a.quantity;
        state.remaining_demand[a.d_idx] += a.quantity;
        state.total_cost -= a.total_cost;
        removed.push(a);
    }
    removed
}

// ---------------------------------------------------------------------------
// Cleanup после destroy: удаление нарушений MIN_BATCH с возвратом назначений
// ---------------------------------------------------------------------------

/// Проверяет все назначения в `state` на соответствие ограничению MIN_BATCH
/// для пар станций массовой выгрузки.
///
/// Удаляет нарушающие назначения, возвращает вагоны в остатки предложения/спроса
/// и **возвращает список освобождённых назначений**, чтобы оператор ремонта
/// мог повторно покрыть высвободившийся спрос.
///
/// Вызывается **после `destroy`** (не после repair): к моменту ремонта
/// состояние уже валидно, и ремонт строит новые назначения с inline-проверкой.
fn drain_violated_mass_pairs(state: &mut AlnsState, arcs: &[TaskArc]) -> Vec<Assignment> {
    let violations: HashSet<(String, String)> = collect_mass_pair_violations(
        state.assignments.iter().map(|a| (a.arc_id, a.quantity)),
        arcs,
    )
    .into_iter()
    .collect();

    if violations.is_empty() {
        return vec![];
    }

    let mut freed: Vec<Assignment> = Vec::new();
    let mut i = state.assignments.len();
    while i > 0 {
        i -= 1;
        let a = &state.assignments[i];
        let arc = &arcs[a.arc_id];
        if arc.is_mass_unloading {
            let pair = (arc.supply_station_code.clone(), arc.demand_station_code.clone());
            if violations.contains(&pair) {
                let removed = state.assignments.swap_remove(i);
                state.remaining_supply[removed.s_idx] += removed.quantity;
                state.remaining_demand[removed.d_idx] += removed.quantity;
                state.total_cost -= removed.total_cost;
                freed.push(removed);
            }
        }
    }
    freed
}

// ---------------------------------------------------------------------------
// Оператор ремонта: жадная реинсерция
// ---------------------------------------------------------------------------

/// Жадно реинсертирует разрушенные узлы обратно в решение.
///
/// Для каждого разрушенного назначения ищет лучшую допустимую дугу
/// с учётом текущих остатков предложения и спроса.
///
/// Ограничение MIN_BATCH проверяется **inline** (по тем же условиям A и B,
/// что и в `greedy_initial_solution`) — пост-удаление не нужно.
///
/// Используется как быстрый оператор ремонта когда LP-ремонт избыточен.
fn repair_greedy(
    state:   &mut AlnsState,
    removed: &[Assignment],
    arcs:    &[TaskArc],
) {
    use std::collections::HashMap;

    // Индекс узлов предложения по парам (supply_station, demand_station)
    // для дуг is_mass_unloading. Позволяет быстро считать station_remaining.
    let mut mass_pair_supply_idx: HashMap<(String, String), HashSet<usize>> = HashMap::new();
    for arc in arcs.iter().filter(|a| a.is_mass_unloading) {
        mass_pair_supply_idx
            .entry((arc.supply_station_code.clone(), arc.demand_station_code.clone()))
            .or_default()
            .insert(arc.s_idx);
    }

    // Текущий суммарный поток по парам (из уже активных назначений в state).
    let mut mass_pair_totals: HashMap<(String, String), i32> = HashMap::new();
    for a in &state.assignments {
        let arc = &arcs[a.arc_id];
        if arc.is_mass_unloading {
            *mass_pair_totals
                .entry((arc.supply_station_code.clone(), arc.demand_station_code.clone()))
                .or_insert(0) += a.quantity;
        }
    }

    // Уникальные d_idx из разрушенных назначений.
    let mut demand_indices: Vec<usize> = removed.iter().map(|a| a.d_idx).collect();
    demand_indices.sort_unstable();
    demand_indices.dedup();

    for d_idx in demand_indices {
        if state.remaining_demand[d_idx] <= 0 { continue; }

        let rem_demand = state.remaining_demand[d_idx];

        // Ищем лучшую дугу с inline MIN_BATCH-проверкой.
        let best_arc = arcs.iter()
            .filter(|arc| {
                if arc.d_idx != d_idx || !arc.car_type_ok { return false; }
                let avail = state.remaining_supply[arc.s_idx];
                if avail <= 0 { return false; }

                if arc.is_mass_unloading {
                    let key = (arc.supply_station_code.clone(), arc.demand_station_code.clone());
                    let existing = mass_pair_totals.get(&key).copied().unwrap_or(0);
                    let station_remaining: i32 = mass_pair_supply_idx
                        .get(&key)
                        .map(|nodes| nodes.iter().map(|&si| state.remaining_supply[si]).sum())
                        .unwrap_or(0);

                    // (A) пара не наберёт MIN_BATCH даже суммарно
                    if existing + station_remaining < MIN_BATCH_FROM_MASS_STATION {
                        return false;
                    }

                    // (B) назначение оставит застрявший остаток < MIN_BATCH
                    let qty = avail.min(rem_demand);
                    let residual = avail - qty;
                    let other_station_remaining = station_remaining - avail;
                    if residual > 0
                        && residual < MIN_BATCH_FROM_MASS_STATION
                        && other_station_remaining < MIN_BATCH_FROM_MASS_STATION
                    {
                        return false;
                    }
                }

                true
            })
            .min_by(|a, b| {
                a.cost.partial_cmp(&b.cost)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.distance.cmp(&b.distance))
            });

        if let Some(arc) = best_arc {
            let qty = state.remaining_supply[arc.s_idx].min(rem_demand);

            let arc_cost = qty as f64 * arc.cost;
            state.remaining_supply[arc.s_idx] -= qty;
            state.remaining_demand[arc.d_idx] -= qty;
            state.total_cost += arc_cost;

            if arc.is_mass_unloading {
                let key = (arc.supply_station_code.clone(), arc.demand_station_code.clone());
                *mass_pair_totals.entry(key).or_insert(0) += qty;
            }

            state.assignments.push(Assignment {
                arc_id:     arc.arc_id,
                s_idx:      arc.s_idx,
                d_idx:      arc.d_idx,
                quantity:   qty,
                total_cost: arc_cost,
            });
        }
    }
    // Нет пост-удаления: inline-проверка гарантирует допустимость назначений.
}

// ---------------------------------------------------------------------------
// Оператор ремонта: LP-подзадача через HiGHS
// ---------------------------------------------------------------------------

/// Извлекает подмножество дуг для LP-ремонта.
///
/// Берём все дуги, связанные с разрушенными узлами предложения/спроса,
/// плюс `NEIGHBOUR_ARCS_PER_NODE` ближайших по стоимости соседей для
/// более широкого контекста переназначения.
fn extract_subproblem_arcs<'a>(
    removed: &[Assignment],
    arcs:    &'a [TaskArc],
) -> Vec<&'a TaskArc> {
    use std::collections::HashSet;

    let s_indices: HashSet<usize> = removed.iter().map(|a| a.s_idx).collect();
    let d_indices: HashSet<usize> = removed.iter().map(|a| a.d_idx).collect();

    // Прямые дуги: касаются разрушенных узлов.
    let mut direct: Vec<&TaskArc> = arcs.iter()
        .filter(|arc| {
            arc.car_type_ok
                && (s_indices.contains(&arc.s_idx) || d_indices.contains(&arc.d_idx))
        })
        .collect();

    // Соседние дуги: для каждого разрушенного d_idx берём N дешевейших
    // дуг из s_idx, которые НЕ вошли в прямые.
    let direct_arc_ids: HashSet<usize> = direct.iter().map(|a| a.arc_id).collect();

    for &d_idx in &d_indices {
        let mut neighbours: Vec<&TaskArc> = arcs.iter()
            .filter(|arc| {
                arc.car_type_ok
                    && arc.d_idx == d_idx
                    && !direct_arc_ids.contains(&arc.arc_id)
            })
            .collect();
        neighbours.sort_unstable_by(|a, b| {
            a.cost.partial_cmp(&b.cost).unwrap_or(std::cmp::Ordering::Equal)
        });
        direct.extend(neighbours.into_iter().take(NEIGHBOUR_ARCS_PER_NODE));
    }

    direct.sort_unstable_by_key(|a| a.arc_id);
    direct.dedup_by_key(|a| a.arc_id);
    direct
}

/// Строит подмножества узлов предложения и спроса для LP-подзадачи,
/// используя только разрушенные узлы с их текущими остатками.
///
/// Возвращает:
/// - `sub_arcs`   — переиндексированные дуги подзадачи
/// - `sub_supply` — узлы предложения подзадачи (остатки из state)
/// - `sub_demand` — узлы спроса подзадачи (остатки из state)
/// - `s_map`      — маппинг sub_s_idx → оригинальный s_idx
/// - `d_map`      — маппинг sub_d_idx → оригинальный d_idx
fn build_subproblem(
    removed:  &[Assignment],
    arcs:     &[TaskArc],
    state:    &AlnsState,
    supply:   &[SupplyNode],
    demand:   &[DemandNode],
) -> (Vec<TaskArc>, Vec<SupplyNode>, Vec<DemandNode>, Vec<usize>, Vec<usize>) {
    use std::collections::HashMap;

    let sub_arcs_refs = extract_subproblem_arcs(removed, arcs);

    // Уникальные s_idx и d_idx в подзадаче.
    let mut s_set: Vec<usize> = sub_arcs_refs.iter().map(|a| a.s_idx).collect();
    s_set.sort_unstable(); s_set.dedup();
    let mut d_set: Vec<usize> = sub_arcs_refs.iter().map(|a| a.d_idx).collect();
    d_set.sort_unstable(); d_set.dedup();

    // Маппинги оригинальный idx → локальный idx.
    let s_local: HashMap<usize, usize> = s_set.iter().enumerate().map(|(i, &s)| (s, i)).collect();
    let d_local: HashMap<usize, usize> = d_set.iter().enumerate().map(|(i, &d)| (d, i)).collect();

    // Переиндексированные дуги подзадачи.
    let sub_arcs: Vec<TaskArc> = sub_arcs_refs.iter().enumerate().map(|(new_id, arc)| {
        TaskArc {
            arc_id:              new_id,
            s_idx:               s_local[&arc.s_idx],
            d_idx:               d_local[&arc.d_idx],
            supply_station_code: arc.supply_station_code.clone(),
            demand_station_code: arc.demand_station_code.clone(),
            cost:                arc.cost,
            distance:            arc.distance,
            delivery_days:       arc.delivery_days,
            period_ok:           arc.period_ok,
            car_type_ok:         arc.car_type_ok,
            is_mass_unloading:   arc.is_mass_unloading,
        }
    }).collect();

    // Узлы предложения с текущими остатками (не оригинальными car_count).
    let sub_supply: Vec<SupplyNode> = s_set.iter().map(|&s_idx| {
        let mut node = supply[s_idx].clone();
        node.car_count = state.remaining_supply[s_idx];
        node
    }).collect();

    // Узлы спроса с текущими остатками.
    let sub_demand: Vec<DemandNode> = d_set.iter().map(|&d_idx| {
        let mut node = demand[d_idx].clone();  // DemandNode должен реализовать Clone
        node.car_count = state.remaining_demand[d_idx];
        node
    }).collect();

    (sub_arcs, sub_supply, sub_demand, s_set, d_set)
}

/// LP-ремонт: решает подзадачу HiGHS и применяет результат к состоянию.
///
/// Ограничение MIN_BATCH проверяется **inline** при применении LP-результата
/// (условия A и B). LP не знает про MIN_BATCH, поэтому проверка идёт на стороне
/// применения: если пара не может набрать MIN_BATCH — назначение пропускается,
/// вагоны остаются в `remaining_supply` для последующих итераций.
///
/// Возвращает `true` если ремонт выполнен успешно.
fn repair_lp(
    state:   &mut AlnsState,
    removed: &[Assignment],
    arcs:    &[TaskArc],
    supply:  &[SupplyNode],
    demand:  &[DemandNode],
) -> bool {
    use std::collections::HashMap;

    let (sub_arcs, sub_supply, sub_demand, s_map, d_map) =
        build_subproblem(removed, arcs, state, supply, demand);

    if sub_arcs.is_empty() { return false; }

    // Индекс оригинальных дуг по (s_idx, d_idx) для поиска флагов и кодов.
    let orig_arc_idx: HashMap<(usize, usize), &TaskArc> = arcs.iter()
        .map(|a| ((a.s_idx, a.d_idx), a))
        .collect();

    let (_, arc_vals) = solve(&sub_arcs, &sub_supply, &sub_demand);

    // Пред-вычисление для inline MIN_BATCH-проверки.
    //
    // mass_pair_supply_idx: (supply_station, demand_station) → множество s_idx.
    let mut mass_pair_supply_idx: HashMap<(String, String), HashSet<usize>> = HashMap::new();
    for arc in arcs.iter().filter(|a| a.is_mass_unloading) {
        mass_pair_supply_idx
            .entry((arc.supply_station_code.clone(), arc.demand_station_code.clone()))
            .or_default()
            .insert(arc.s_idx);
    }

    // mass_pair_totals: суммарный поток по парам из уже активных назначений.
    // Обновляется по мере применения LP-результата.
    let mut mass_pair_totals: HashMap<(String, String), i32> = HashMap::new();
    for a in &state.assignments {
        let arc = &arcs[a.arc_id];
        if arc.is_mass_unloading {
            *mass_pair_totals
                .entry((arc.supply_station_code.clone(), arc.demand_station_code.clone()))
                .or_insert(0) += a.quantity;
        }
    }

    // Применяем результат LP с inline MIN_BATCH-проверкой.
    for (arc, &qty_f) in sub_arcs.iter().zip(arc_vals.iter()) {
        let qty_lp = qty_f.round() as i32;
        if qty_lp <= 0 { continue; }

        let orig_s = s_map[arc.s_idx];
        let orig_d = d_map[arc.d_idx];

        let orig_arc = orig_arc_idx.get(&(orig_s, orig_d)).copied();
        let orig_arc_id = orig_arc.map(|a| a.arc_id).unwrap_or(arc.arc_id);
        let is_mass = orig_arc.map(|a| a.is_mass_unloading).unwrap_or(arc.is_mass_unloading);

        // Фактически доступные остатки (LP мог не знать об изменениях в ходе цикла).
        let avail_supply = state.remaining_supply[orig_s];
        let avail_demand = state.remaining_demand[orig_d];
        let qty = qty_lp.min(avail_supply).min(avail_demand);
        if qty <= 0 { continue; }

        if is_mass {
            let supply_code = orig_arc
                .map(|a| a.supply_station_code.clone())
                .unwrap_or_else(|| arc.supply_station_code.clone());
            let demand_code = orig_arc
                .map(|a| a.demand_station_code.clone())
                .unwrap_or_else(|| arc.demand_station_code.clone());
            let key = (supply_code, demand_code);

            let existing = mass_pair_totals.get(&key).copied().unwrap_or(0);
            let station_remaining: i32 = mass_pair_supply_idx
                .get(&key)
                .map(|nodes| nodes.iter().map(|&si| state.remaining_supply[si]).sum())
                .unwrap_or(0);

            // (A) пара не наберёт MIN_BATCH даже суммарно — пропускаем.
            if existing + station_remaining < MIN_BATCH_FROM_MASS_STATION {
                continue;
            }

            // (B) назначение оставит застрявший остаток < MIN_BATCH — пропускаем.
            let residual = avail_supply - qty;
            let other_station_remaining = station_remaining - avail_supply;
            if residual > 0
                && residual < MIN_BATCH_FROM_MASS_STATION
                && other_station_remaining < MIN_BATCH_FROM_MASS_STATION
            {
                continue;
            }

            let arc_cost = qty as f64 * arc.cost;
            state.remaining_supply[orig_s] -= qty;
            state.remaining_demand[orig_d] -= qty;
            state.total_cost += arc_cost;
            *mass_pair_totals.entry(key).or_insert(0) += qty;

            state.assignments.push(Assignment {
                arc_id:     orig_arc_id,
                s_idx:      orig_s,
                d_idx:      orig_d,
                quantity:   qty,
                total_cost: arc_cost,
            });
        } else {
            let arc_cost = qty as f64 * arc.cost;
            state.remaining_supply[orig_s] -= qty;
            state.remaining_demand[orig_d] -= qty;
            state.total_cost += arc_cost;

            state.assignments.push(Assignment {
                arc_id:     orig_arc_id,
                s_idx:      orig_s,
                d_idx:      orig_d,
                quantity:   qty,
                total_cost: arc_cost,
            });
        }
    }
    // Нет пост-удаления: inline-проверка гарантирует допустимость назначений.
    true
}

// ---------------------------------------------------------------------------
// Главный цикл ALNS
// ---------------------------------------------------------------------------

/// Запускает ALNS поверх жадного начального решения.
///
/// # Стратегия
/// ```text
/// 1. Инициализация: жадное решение → AlnsState
/// 2. Цикл (пока time_budget не исчерпан):
///    a. Destroy: случайно удалить K назначений
///    b. Repair:  LP-подзадача на разрушенных узлах + соседях
///    c. Accept:  принять если new_cost < best_cost
///    d. Adapt:   увеличить K если стагнация, уменьшить если улучшение
/// 3. Вернуть лучшее состояние
/// ```
pub fn run_alns(
    greedy:  &GreedyResult,
    arcs:    &[TaskArc],
    supply:  &[SupplyNode],
    demand:  &[DemandNode],
    config:  &AlnsConfig,
) -> AlnsResult {
    let start = Instant::now();

    let mut rng: StdRng = match config.seed {
        Some(s) => StdRng::seed_from_u64(s),
        None    => StdRng::from_entropy(),
    };

    // --- Инициализация ---
    let initial_state = AlnsState::from_greedy(greedy, supply, demand);
    let mut best_state   = initial_state.clone();
    let mut current_state = initial_state;

    let mut destroy_ratio = config.destroy_ratio;
    let mut iters_no_improvement: usize = 0;

    let mut stats = AlnsStats {
        iterations:          0,
        improvements:        0,
        cost_history:        vec![best_state.total_cost],
        elapsed:             Duration::ZERO,
        final_destroy_ratio: destroy_ratio,
    };

    println!("--- ALNS СТАРТ ---");
    let (start_unmet, start_excess) = best_state.unmet_and_excess(demand);
    println!("Начальная real_cost:      {:.2} руб.", best_state.total_cost);
    println!(
        "Начальная objective_cost: {:.2} руб. (penalty: {:.2}, unmet: {}, excess: {})",
        best_state.objective_cost(demand),
        best_state.penalty_component_cost(demand),
        start_unmet,
        start_excess,
    );
    println!("Назначений:          {}", best_state.assignments.len());
    println!("Бюджет времени:      {} сек.", config.time_budget.as_secs());
    println!("------------------");

    // --- Главный цикл ---
    while start.elapsed() < config.time_budget {
        stats.iterations += 1;

        // Количество разрушаемых назначений.
        let k = ((current_state.assignments.len() as f64 * destroy_ratio) as usize).max(1);

        // Клонируем текущее состояние для попытки.
        let mut candidate = current_state.clone();

        // --- DESTROY ---
        let mut removed = destroy_random(&mut candidate, k, &mut rng);
        if removed.is_empty() { continue; }

        // Разрушение могло опустить суммарный поток по парам массовой выгрузки
        // ниже MIN_BATCH. Освобождаем такие пары и добавляем их d_idx в список
        // разрушенных, чтобы оператор ремонта повторно покрыл высвободившийся спрос.
        let freed_by_cleanup = drain_violated_mass_pairs(&mut candidate, arcs);
        removed.extend(freed_by_cleanup);

        // --- REPAIR ---
        // Пробуем LP-ремонт; если не удался — откатываемся к жадному.
        let repaired = repair_lp(&mut candidate, &removed, arcs, supply, demand);
        if !repaired {
            repair_greedy(&mut candidate, &removed, arcs);
        }

        candidate.recalculate_cost();

        // --- ACCEPT (только если лучше) ---
        let candidate_obj = candidate.objective_cost(demand);
        let best_obj = best_state.objective_cost(demand);

        let candidate_assigned: i32 = candidate.assignments.iter().map(|a| a.quantity).sum();
        let best_assigned: i32 = best_state.assignments.iter().map(|a| a.quantity).sum();

        let accept = if candidate_obj + 1e-6 < best_obj {
            true
        } else if (candidate_obj - best_obj).abs() <= 1e-6 {
            // Tie-break: при равной цели предпочитаем большее покрытие, затем меньшую реальную стоимость.
            (candidate_assigned > best_assigned)
                || (candidate_assigned == best_assigned && candidate.total_cost < best_state.total_cost)
        } else {
            false
        };

        if accept {
            let improvement = best_obj - candidate_obj;
            best_state    = candidate.clone();
            current_state = candidate;

            stats.improvements        += 1;
            iters_no_improvement       = 0;

            // Адаптация K вниз: нашли улучшение — сужаем окрестность.
            destroy_ratio = (destroy_ratio - DESTROY_RATIO_STEP_DOWN)
                .max(DESTROY_RATIO_MIN);

            println!(
                "[iter {:>5}] ✓ objective -{:.2} | real {:.2} | penalty {:.2} | unmet {} | excess {} | K={:.0}%",
                stats.iterations,
                improvement,
                best_state.total_cost,
                best_state.penalty_component_cost(demand),
                best_state.unmet_and_excess(demand).0,
                best_state.unmet_and_excess(demand).1,
                destroy_ratio * 100.0,
            );
        } else {
            iters_no_improvement += 1;

            // Адаптация K вверх: стагнация — расширяем окрестность.
            if iters_no_improvement >= STAGNATION_THRESHOLD {
                destroy_ratio = (destroy_ratio + DESTROY_RATIO_STEP_UP)
                    .min(DESTROY_RATIO_MAX);
                iters_no_improvement = 0;
            }
        }

        // Журнал каждые 10 итераций.
        if stats.iterations % 10 == 0 {
            stats.cost_history.push(best_state.total_cost);
        }
    }

    stats.elapsed             = start.elapsed();
    stats.final_destroy_ratio = destroy_ratio;

    let unmet_load_fin: i32 = best_state
        .remaining_demand
        .iter()
        .zip(demand.iter())
        .filter(|(r, d)| d.purpose == DemandPurpose::Load && **r > 0)
        .map(|(r, _)| *r)
        .sum();
    let excess_fin: i32 = best_state
        .remaining_supply
        .iter()
        .filter(|&&s| s > 0)
        .sum();

    let arc_vals = greedy_to_arc_vals(
        &GreedyResult {
            assignments:   best_state.assignments.clone(),
            total_cost:    best_state.total_cost,
            assigned_cars: best_state.assignments.iter().map(|a| a.quantity).sum(),
            unmet_demand:  unmet_load_fin,
            excess_supply: excess_fin,
        },
        arcs.len(),
    );

    println!("--- ALNS ФИНИШ ---");
    println!("Итераций:            {}", stats.iterations);
    println!("Улучшений:           {}", stats.improvements);
    let (final_unmet, final_excess) = best_state.unmet_and_excess(demand);
    println!("Лучшая real_cost:    {:.2} руб.", best_state.total_cost);
    println!(
        "Лучшая objective:    {:.2} руб. (penalty: {:.2}, unmet: {}, excess: {})",
        best_state.objective_cost(demand),
        best_state.penalty_component_cost(demand),
        final_unmet,
        final_excess,
    );
    println!("Затрачено:           {:.1} сек.", stats.elapsed.as_secs_f64());
    println!("Финальный K:         {:.0}%", stats.final_destroy_ratio * 100.0);
    println!("------------------");

    AlnsResult { best_state, arc_vals, stats }
}

// ---------------------------------------------------------------------------
// Точка входа: запуск полного пайплайна
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Конвертация результата ALNS → OptimResult
// ---------------------------------------------------------------------------

impl AlnsResult {
    /// Конвертирует лучшее состояние ALNS в `OptimResult` для отчёта и вывода.
    pub fn to_optim_result(&self, demand: &[DemandNode]) -> OptimResult {
        let assigned_cars: f64 = self.best_state.assignments.iter()
            .map(|a| a.quantity as f64).sum();
        let penalty_cars: f64 = self
            .best_state
            .remaining_demand
            .iter()
            .zip(demand.iter())
            .filter(|(r, d)| d.purpose == DemandPurpose::Load && **r > 0)
            .map(|(r, _)| *r as f64)
            .sum();
        let excess_supply: f64 = self.best_state.remaining_supply.iter()
            .filter(|&&s| s > 0).sum::<i32>() as f64;

        OptimResult {
            total_cost: self.best_state.total_cost,
            assigned_cars,
            penalty_cars,
            excess_supply,
            status: format!(
                "ALNS ({} итер., {} улучш., {:.1} сек.)",
                self.stats.iterations,
                self.stats.improvements,
                self.stats.elapsed.as_secs_f64(),
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Точка входа: запуск полного пайплайна
// ---------------------------------------------------------------------------

/// Запускает полный пайплайн: жадное решение → ALNS.
///
/// Используй эту функцию вместо прямого вызова `solve()` для крупных задач.
pub fn solve_with_alns(
    arcs:   &[TaskArc],
    supply: &[SupplyNode],
    demand: &[DemandNode],
    config: &AlnsConfig,
) -> AlnsResult {
    use super::greedy::{greedy_initial_solution, print_greedy_result};
    use super::lp::print_balance;

    print_balance(supply, demand);

    let greedy = greedy_initial_solution(arcs, supply, demand);
    print_greedy_result(&greedy, supply, demand);

    run_alns(&greedy, arcs, supply, demand, config)
}

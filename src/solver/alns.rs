use std::collections::HashSet;
use std::time::{Duration, Instant};
use rand::prelude::*;

use crate::node::{DemandNode, DemandPurpose, SupplyNode};
use super::model::{collect_pair_min_batch_violations, DmziIndex, DmziLimits, PairKey, TaskArc};
use super::greedy::{Assignment, GreedyResult, greedy_to_arc_vals};
use super::lp::{solve, OptimResult, PENALTY_EXCESS, PENALTY_UNMET};
use super::mip::solve_mip;

// ---------------------------------------------------------------------------
// Квоты ДМЗИ внутри ALNS
// ---------------------------------------------------------------------------

/// Остатки квот ДМЗИ для текущего состояния: лимиты бакетов за вычетом потока
/// всех активных назначений state (по `arc_id` в полном списке `arcs`).
fn dmzi_remaining_for_state(index: &DmziIndex, assignments: &[Assignment]) -> Vec<i32> {
    let mut rem = index.limits_vec();
    for a in assignments {
        if let Some(b) = index.arc_bucket[a.arc_id] {
            rem[b] -= a.quantity;
        }
    }
    rem
}

// ---------------------------------------------------------------------------
// Константы
// ---------------------------------------------------------------------------

/// Бюджет времени ALNS по умолчанию.
const DEFAULT_TIME_BUDGET: Duration = Duration::from_secs(180); // 3 минуты

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

/// Бюджет времени MIP-подзадачи в операторе `repair_mip`.
///
/// Должен быть значительно меньше лимита главного MIP-решателя: подзадача
/// локальна и обычно решается за доли секунды. Значение 3 с — с запасом
/// на случай большой окрестности разрушения.
const ALNS_MIP_TIME_LIMIT: Duration = Duration::from_secs(3);

/// Целевой относительный разрыв MIP-подзадачи в `repair_mip`.
///
/// 2% — компромисс между скоростью одной итерации ALNS и качеством ремонта.
/// Главный MIP использует 0.5%, но там мы готовы потратить больше времени.
const ALNS_MIP_REL_GAP: f64 = 0.02;

/// Минимальное улучшение полной целевой функции (руб.), при котором кандидат
/// принимается. Отсекает шум округления `f64` при пересчёте `total_cost`.
const ACCEPT_MIN_IMPROVEMENT_RUB: f64 = 1.0;

/// Критерий приёма кандидата ALNS.
///
/// Кандидат принимается, если он **не увеличивает** незакрытый Load-спрос и
/// **строго уменьшает** полную целевую функцию
/// `objective = real_cost + PENALTY_UNMET·unmet + PENALTY_EXCESS·excess`
/// (та же цель, что у главного MIP).
///
/// # Почему не лексикографика `(unmet + excess, unmet, real_cost)`
///
/// Прежний критерий принимал кандидата при **любом** уменьшении `excess`,
/// не глядя на стоимость. `repair_mip` работает 3 с на подзадаче из десятков
/// тысяч дуг, и при `ReachedTimeLimit` возвращает первый попавшийся incumbent.
/// На прогоне 07.09.2026 такой incumbent пристроил 2 вагона в промывку
/// (`excess 2077 → 2075`), одновременно пересадив ~100 вагонов с дуг по 50 тыс.
/// на дуги по 350–400 тыс.: `real_cost 258 → 293 млн` (+35 млн). Критерий
/// принял это как «улучшение» — MIP-оптимум был разрушен, в выгрузке появились
/// подсылы ДВС → центр при ближних вагонах в отстое.
///
/// По полной целевой функции этот обмен = `−2·5 000 + 35 000 000` — отклоняется.
/// При этом обмен «unmet −1, excess +1» (`−1 000 000 + 5 000`) принимается:
/// закрыть заявку важнее, чем избавиться от вагона в остатке — ровно так же,
/// как это трактует MIP.
///
/// Дополнительный жёсткий запрет на рост `unmet` — бизнес-приоритет покрытия:
/// экономия на плечах никогда не должна оплачиваться незакрытой заявкой.
pub fn accept_candidate(cand_unmet: i32, best_unmet: i32, cand_obj: f64, best_obj: f64) -> bool {
    cand_unmet <= best_unmet && cand_obj + ACCEPT_MIN_IMPROVEMENT_RUB <= best_obj
}

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

    /// Полная целевая функция, согласованная с LP/MIP:
    /// стоимость реальных дуг + PENALTY_UNMET * unmet_demand + PENALTY_EXCESS * excess_supply.
    pub fn objective_cost(&self, demand: &[DemandNode]) -> f64 {
        let (unmet_demand, excess_supply) = self.unmet_and_excess(demand);
        self.total_cost
            + PENALTY_UNMET * unmet_demand as f64
            + PENALTY_EXCESS * excess_supply as f64
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
        PENALTY_UNMET * unmet_demand as f64 + PENALTY_EXCESS * excess_supply as f64
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
    /// Использовать MIP-ремонт (жёсткий MIN_BATCH) вместо LP-ремонта.
    ///
    /// При `true` подзадача решается полноценным MIP — MIN_BATCH учитывается
    /// в модели, дуги не отбрасываются на этапе применения решения. Рекомендуется
    /// при наличии пар массовой выгрузки. При `false` используется быстрый LP-ремонт
    /// с inline-проверками (может терять назначения на парах MIN_BATCH).
    pub use_mip_repair: bool,
}

impl Default for AlnsConfig {
    fn default() -> Self {
        AlnsConfig {
            time_budget:    DEFAULT_TIME_BUDGET,
            destroy_ratio:  DESTROY_RATIO_INIT,
            seed:           None,
            use_mip_repair: true,
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

/// Проверяет все назначения в `state` на соответствие ограничению минимальной партии
/// для групп с `pair_min_batch > 0` (массовая выгрузка, средние станции, маршрутные).
///
/// Удаляет нарушающие назначения, возвращает вагоны в остатки предложения/спроса
/// и **возвращает список освобождённых назначений**, чтобы оператор ремонта
/// мог повторно покрыть высвободившийся спрос.
///
/// Вызывается **после `destroy`** (не после repair): к моменту ремонта
/// состояние уже валидно, и ремонт строит новые назначения с inline-проверкой.
fn drain_violated_mass_pairs(state: &mut AlnsState, arcs: &[TaskArc]) -> Vec<Assignment> {
    let violations: HashSet<PairKey> = collect_pair_min_batch_violations(
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
        if arc.has_pair_min_batch() {
            let pair = arc.pair_key();
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
/// Квоты ДМЗИ также учитываются inline: дуги с исчерпанным бакетом отбрасываются,
/// объём назначения клиппится остатком квоты.
///
/// Используется как быстрый оператор ремонта когда LP-ремонт избыточен.
fn repair_greedy(
    state:   &mut AlnsState,
    removed: &[Assignment],
    arcs:    &[TaskArc],
    dmzi:    Option<&DmziIndex>,
) {
    use std::collections::HashMap;

    // Остатки квот ДМЗИ с учётом всех активных назначений state.
    let mut dmzi_rem: Vec<i32> = dmzi
        .map(|idx| dmzi_remaining_for_state(idx, &state.assignments))
        .unwrap_or_default();
    let bucket_of = |arc_id: usize| -> Option<usize> {
        dmzi.and_then(|idx| idx.arc_bucket[arc_id])
    };

    // Индекс узлов предложения по группам pair_key для дуг с pair_min_batch > 0.
    // Позволяет быстро считать station_remaining.
    let mut mass_pair_supply_idx: HashMap<PairKey, HashSet<usize>> = HashMap::new();
    for arc in arcs.iter().filter(|a| a.has_pair_min_batch()) {
        mass_pair_supply_idx
            .entry(arc.pair_key())
            .or_default()
            .insert(arc.s_idx);
    }

    // Текущий суммарный поток по группам (из уже активных назначений в state).
    let mut mass_pair_totals: HashMap<PairKey, i32> = HashMap::new();
    for a in &state.assignments {
        let arc = &arcs[a.arc_id];
        if arc.has_pair_min_batch() {
            *mass_pair_totals.entry(arc.pair_key()).or_insert(0) += a.quantity;
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

                // Квота ДМЗИ бакета дуги исчерпана — дуга недоступна.
                if let Some(b) = bucket_of(arc.arc_id) {
                    if dmzi_rem[b] <= 0 { return false; }
                }

                if arc.has_pair_min_batch() {
                    let b = arc.pair_min_batch;
                    let key = arc.pair_key();
                    let existing = mass_pair_totals.get(&key).copied().unwrap_or(0);
                    let station_remaining: i32 = mass_pair_supply_idx
                        .get(&key)
                        .map(|nodes| nodes.iter().map(|&si| state.remaining_supply[si]).sum())
                        .unwrap_or(0);

                    // (A) пара не наберёт порог партии даже суммарно
                    if existing + station_remaining < b {
                        return false;
                    }

                    // (B) назначение оставит застрявший остаток < порога партии
                    let qty = avail.min(rem_demand);
                    let residual = avail - qty;
                    let other_station_remaining = station_remaining - avail;
                    if residual > 0 && residual < b && other_station_remaining < b {
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
            let mut qty = state.remaining_supply[arc.s_idx].min(rem_demand);
            if let Some(b) = bucket_of(arc.arc_id) {
                qty = qty.min(dmzi_rem[b]);
                dmzi_rem[b] -= qty;
            }
            if qty <= 0 { continue; }

            let arc_cost = qty as f64 * arc.cost;
            state.remaining_supply[arc.s_idx] -= qty;
            state.remaining_demand[arc.d_idx] -= qty;
            state.total_cost += arc_cost;

            if arc.has_pair_min_batch() {
                *mass_pair_totals.entry(arc.pair_key()).or_insert(0) += qty;
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
            pair_min_batch:      arc.pair_min_batch,
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

/// Warm-start MIP-подзадачи из разрушенных назначений.
///
/// Возвращает вектор длины `sub_arcs.len()`: для каждой дуги подзадачи — суммарное
/// количество вагонов удалённых назначений по той же паре `(s_idx, d_idx)`
/// в оригинальной индексации (`s_map`/`d_map` — локальный → оригинальный индекс).
/// Назначения, чья пара в подзадачу не попала (в норме невозможно: `extract_subproblem_arcs`
/// берёт все дуги разрушенных узлов), просто игнорируются.
///
/// Пары, чей поток после destroy/drain оказался `0 < sum < B_pair`, обнуляются
/// санацией warm-start внутри [`solve_mip`].
fn warm_start_from_removed(
    removed:  &[Assignment],
    sub_arcs: &[TaskArc],
    s_map:    &[usize],
    d_map:    &[usize],
) -> Vec<f64> {
    use std::collections::HashMap;

    let by_pair: HashMap<(usize, usize), usize> = sub_arcs
        .iter()
        .enumerate()
        .map(|(i, a)| ((s_map[a.s_idx], d_map[a.d_idx]), i))
        .collect();

    let mut warm = vec![0.0_f64; sub_arcs.len()];
    for a in removed {
        if let Some(&i) = by_pair.get(&(a.s_idx, a.d_idx)) {
            warm[i] += a.quantity as f64;
        }
    }
    warm
}

/// LP-ремонт: решает подзадачу HiGHS и применяет результат к состоянию.
///
/// Ограничение MIN_BATCH проверяется **inline** при применении LP-результата
/// (условия A и B). LP не знает про MIN_BATCH, поэтому проверка идёт на стороне
/// применения: если пара не может набрать MIN_BATCH — назначение пропускается,
/// вагоны остаются в `remaining_supply` для последующих итераций.
/// Квоты ДМЗИ также применяются inline: объём клиппится остатком квоты бакета.
///
/// Возвращает `true` если ремонт выполнен успешно.
fn repair_lp(
    state:   &mut AlnsState,
    removed: &[Assignment],
    arcs:    &[TaskArc],
    supply:  &[SupplyNode],
    demand:  &[DemandNode],
    dmzi:    Option<&DmziIndex>,
) -> bool {
    use std::collections::HashMap;

    let (sub_arcs, sub_supply, sub_demand, s_map, d_map) =
        build_subproblem(removed, arcs, state, supply, demand);

    if sub_arcs.is_empty() { return false; }

    // Остатки квот ДМЗИ с учётом всех активных назначений state.
    let mut dmzi_rem: Vec<i32> = dmzi
        .map(|idx| dmzi_remaining_for_state(idx, &state.assignments))
        .unwrap_or_default();

    // Индекс оригинальных дуг по (s_idx, d_idx) для поиска флагов и кодов.
    let orig_arc_idx: HashMap<(usize, usize), &TaskArc> = arcs.iter()
        .map(|a| ((a.s_idx, a.d_idx), a))
        .collect();

    let (_, arc_vals) = solve(&sub_arcs, &sub_supply, &sub_demand);

    // Пред-вычисление для inline-проверки минимальной партии.
    //
    // mass_pair_supply_idx: pair_key → множество s_idx группы.
    let mut mass_pair_supply_idx: HashMap<PairKey, HashSet<usize>> = HashMap::new();
    for arc in arcs.iter().filter(|a| a.has_pair_min_batch()) {
        mass_pair_supply_idx
            .entry(arc.pair_key())
            .or_default()
            .insert(arc.s_idx);
    }

    // mass_pair_totals: суммарный поток по группам из уже активных назначений.
    // Обновляется по мере применения LP-результата.
    let mut mass_pair_totals: HashMap<PairKey, i32> = HashMap::new();
    for a in &state.assignments {
        let arc = &arcs[a.arc_id];
        if arc.has_pair_min_batch() {
            *mass_pair_totals.entry(arc.pair_key()).or_insert(0) += a.quantity;
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
        let pair_b = orig_arc.map(|a| a.pair_min_batch).unwrap_or(arc.pair_min_batch);

        // Бакет квоты ДМЗИ дуги (по оригинальной дуге; иначе — по узлам напрямую).
        let dmzi_bucket: Option<usize> = dmzi.and_then(|idx| match orig_arc {
            Some(a) => idx.arc_bucket[a.arc_id],
            None => {
                let d = &demand[orig_d];
                if d.purpose == DemandPurpose::Load {
                    idx.bucket_for(&d.railway_name, supply[orig_s].supply_period)
                } else {
                    None
                }
            }
        });

        // Фактически доступные остатки (LP мог не знать об изменениях в ходе цикла).
        let avail_supply = state.remaining_supply[orig_s];
        let avail_demand = state.remaining_demand[orig_d];
        let mut qty = qty_lp.min(avail_supply).min(avail_demand);
        if let Some(b) = dmzi_bucket {
            qty = qty.min(dmzi_rem[b]);
        }
        if qty <= 0 { continue; }

        if pair_b > 0 {
            let key: PairKey = orig_arc
                .map(|a| a.pair_key())
                .unwrap_or_else(|| arc.pair_key());

            let existing = mass_pair_totals.get(&key).copied().unwrap_or(0);
            let station_remaining: i32 = mass_pair_supply_idx
                .get(&key)
                .map(|nodes| nodes.iter().map(|&si| state.remaining_supply[si]).sum())
                .unwrap_or(0);

            // (A) пара не наберёт порог партии даже суммарно — пропускаем.
            if existing + station_remaining < pair_b {
                continue;
            }

            // (B) назначение оставит застрявший остаток < порога партии — пропускаем.
            let residual = avail_supply - qty;
            let other_station_remaining = station_remaining - avail_supply;
            if residual > 0 && residual < pair_b && other_station_remaining < pair_b {
                continue;
            }

            let arc_cost = qty as f64 * arc.cost;
            state.remaining_supply[orig_s] -= qty;
            state.remaining_demand[orig_d] -= qty;
            state.total_cost += arc_cost;
            *mass_pair_totals.entry(key).or_insert(0) += qty;
            if let Some(b) = dmzi_bucket {
                dmzi_rem[b] -= qty;
            }

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
            if let Some(b) = dmzi_bucket {
                dmzi_rem[b] -= qty;
            }

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
// Оператор ремонта: MIP-подзадача через HiGHS (жёсткий MIN_BATCH)
// ---------------------------------------------------------------------------

/// MIP-ремонт: решает подзадачу как MIP с big-M формулировкой MIN_BATCH.
///
/// В отличие от [`repair_lp`], MIN_BATCH встроен прямо в модель подзадачи через
/// бинарные переменные — HiGHS не возвращает дробные/невалидные назначения,
/// поэтому inline-пост-проверки не нужны, а дуги не «теряются» на этапе применения.
///
/// # Корректность относительно внешнего state
/// Подзадача видит `sub_supply.car_count = remaining_supply` (уже учтены назначения
/// state **вне** подзадачи). Применяется правило:
///
/// - **Пары без внешнего потока** (`state_flow == 0`): подзадача использует
///   стандартный `B_pair = MIN_BATCH` и корректно запрещает поток `[1, MIN_BATCH)`.
/// - **Пары с внешним потоком `≥ MIN_BATCH`** (остаток после destroy/drain):
///   в [`solve_mip`] передаётся override `B_pair = 0`. Это означает: подзадача
///   вправе добавить к паре **любое** количество вагонов (в т.ч. `1..MIN_BATCH-1`),
///   потому что суммарный поток пары уже `≥ MIN_BATCH` за счёт внешних
///   назначений в state. Без этого override MIP-LNS систематически «теряет»
///   вагоны на парах, где внешний поток уже покрыл MIN_BATCH.
/// - **Пары с внешним потоком `0 < flow < MIN_BATCH`** невозможны: такое состояние
///   `drain_violated_mass_pairs` эвакуирует сразу после `destroy`, поэтому к моменту
///   вызова `repair_mip` все пары state — либо пустые, либо `≥ MIN_BATCH`.
///
/// # Квоты ДМЗИ
/// Подзадаче передаются **редуцированные** лимиты: из лимита каждого бакета
/// вычитается поток активных назначений state (внешних по отношению к подзадаче;
/// удалённые destroy/drain назначения уже возвращены в остатки и в state не входят).
/// Так суммарный поток state + подзадачи не превышает исходных квот.
///
/// Возвращает `true`, если MIP нашёл хотя бы допустимое решение; `false` —
/// если решатель завершился без пригодного incumbent (сигнал для fallback
/// на `repair_greedy`).
#[allow(clippy::too_many_arguments)]
fn repair_mip(
    state:      &mut AlnsState,
    removed:    &[Assignment],
    arcs:       &[TaskArc],
    supply:     &[SupplyNode],
    demand:     &[DemandNode],
    time_limit: Duration,
    rel_gap:    f64,
    dmzi:       Option<&DmziIndex>,
) -> bool {
    use std::collections::HashMap;

    // Редуцированные квоты ДМЗИ для подзадачи: лимит − поток внешнего state.
    let dmzi_reduced: Option<DmziLimits> = dmzi.map(|idx| {
        let rem = dmzi_remaining_for_state(idx, &state.assignments);
        idx.buckets
            .iter()
            .zip(rem.iter())
            .map(|((key, _), &r)| (key.clone(), r.max(0)))
            .collect()
    });

    // Суммарный поток по каждой группе с pair_min_batch > 0 во внешнем state
    // (до подзадачи). Порог группы — третий элемент ключа. Нужен, чтобы не
    // навязывать подзадаче минимальную партию на группах, где state уже
    // обеспечил её внешними назначениями.
    let mut state_flow: HashMap<PairKey, i32> = HashMap::new();
    for a in &state.assignments {
        let arc = &arcs[a.arc_id];
        if arc.has_pair_min_batch() {
            *state_flow.entry(arc.pair_key()).or_insert(0) += a.quantity;
        }
    }

    // Override для solve_mip: B_pair = 0 для групп, где state_flow ≥ порога группы.
    let pair_override: HashMap<PairKey, i32> = state_flow
        .iter()
        .filter(|&(key, &flow)| flow >= key.2)
        .map(|(key, _)| (key.clone(), 0))
        .collect();

    let (sub_arcs, sub_supply, sub_demand, s_map, d_map) =
        build_subproblem(removed, arcs, state, supply, demand);

    if sub_arcs.is_empty() {
        return false;
    }

    // Индекс оригинальных дуг по (s_idx, d_idx) — чтобы восстановить исходные
    // arc_id и флаги при применении решения.
    let orig_arc_idx: HashMap<(usize, usize), &TaskArc> = arcs.iter()
        .map(|a| ((a.s_idx, a.d_idx), a))
        .collect();

    // Warm-start подзадачи = разрушенные назначения «как были». Это допустимое
    // решение подзадачи (state до destroy удовлетворял всем ограничениям), поэтому
    // HiGHS стартует с incumbent не хуже исходного состояния и за 3 с ищет только
    // улучшения. Без warm-start при `ReachedTimeLimit` подзадача возвращала первый
    // найденный incumbent произвольного качества — источник резких ухудшений плана.
    let warm = warm_start_from_removed(removed, &sub_arcs, &s_map, &d_map);

    let outcome = solve_mip(
        &sub_arcs, &sub_supply, &sub_demand,
        time_limit, Some(&warm), Some(rel_gap),
        Some(&pair_override),
        dmzi_reduced.as_ref(),
    );

    if !outcome.has_feasible_solution() {
        return false;
    }

    // MIP уже обеспечил MIN_BATCH на уровне модели — применяем результат «как есть»
    // без inline-проверок. Клиппинг по остаткам нужен только на случай округления
    // дробных значений (для целочисленных переменных он в норме никогда не срабатывает,
    // но сохраняем как защиту от погрешностей решателя).
    for (arc, &qty_f) in sub_arcs.iter().zip(outcome.arc_vals.iter()) {
        let qty_mip = qty_f.round() as i32;
        if qty_mip <= 0 {
            continue;
        }

        let orig_s = s_map[arc.s_idx];
        let orig_d = d_map[arc.d_idx];

        let orig_arc = orig_arc_idx.get(&(orig_s, orig_d)).copied();
        let orig_arc_id = orig_arc.map(|a| a.arc_id).unwrap_or(arc.arc_id);

        let avail_supply = state.remaining_supply[orig_s];
        let avail_demand = state.remaining_demand[orig_d];
        let qty = qty_mip.min(avail_supply).min(avail_demand);
        if qty <= 0 {
            continue;
        }

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
///    b. Repair:  MIP/LP-подзадача на разрушенных узлах + соседях
///                (MIP — с warm-start из разрушенных назначений)
///    c. Accept:  принять, если objective (real_cost + штрафы) строго уменьшилась
///                и unmet не вырос — см. `accept_candidate`
///    d. Adapt:   увеличить K если стагнация, уменьшить если улучшение
/// 3. Вернуть лучшее состояние
/// ```
pub fn run_alns(
    greedy:  &GreedyResult,
    arcs:    &[TaskArc],
    supply:  &[SupplyNode],
    demand:  &[DemandNode],
    config:  &AlnsConfig,
    dmzi_limits: Option<&DmziLimits>,
) -> AlnsResult {
    let start = Instant::now();

    let mut rng: StdRng = match config.seed {
        Some(s) => StdRng::seed_from_u64(s),
        None    => StdRng::from_entropy(),
    };

    // Индекс квот ДМЗИ по полному списку дуг (общий для всех операторов ремонта).
    let dmzi_index = dmzi_limits
        .filter(|l| !l.is_empty())
        .map(|l| DmziIndex::build(arcs, supply, demand, l));

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
    let start_total = start_unmet + start_excess;
    println!("Начальная real_cost:       {:.2} руб.", best_state.total_cost);
    println!(
        "Начальная objective_cost:  {:.2} руб. (penalty: {:.2}, unmet: {}, excess: {})",
        best_state.objective_cost(demand),
        best_state.penalty_component_cost(demand),
        start_unmet,
        start_excess,
    );
    println!("Нераспределено (итого):    {} ваг. (= unmet + excess)", start_total);
    println!("Назначений:                {}", best_state.assignments.len());
    println!(
        "Ремонтный оператор:        {}",
        if config.use_mip_repair { "repair_mip (жёсткий MIN_BATCH)" } else { "repair_lp (inline-проверки)" }
    );
    println!("Бюджет времени:            {} сек.", config.time_budget.as_secs());
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
        // MIP-ремонт (жёсткий MIN_BATCH) если включён; иначе LP-ремонт с inline-проверками.
        // При неудаче любого из них — жадный fallback.
        let repaired = if config.use_mip_repair {
            repair_mip(
                &mut candidate, &removed, arcs, supply, demand,
                ALNS_MIP_TIME_LIMIT, ALNS_MIP_REL_GAP,
                dmzi_index.as_ref(),
            )
        } else {
            repair_lp(&mut candidate, &removed, arcs, supply, demand, dmzi_index.as_ref())
        };
        if !repaired {
            repair_greedy(&mut candidate, &removed, arcs, dmzi_index.as_ref());
        }

        candidate.recalculate_cost();

        // --- ACCEPT ---
        // Полная целевая функция (real_cost + штрафы), согласованная с главным MIP,
        // плюс запрет на рост незакрытого Load-спроса. Обоснование и разбор
        // инцидента 07.09.2026 — в документации к `accept_candidate`.
        let (cand_unmet, _) = candidate.unmet_and_excess(demand);
        let (best_unmet, _) = best_state.unmet_and_excess(demand);
        let candidate_obj = candidate.objective_cost(demand);
        let best_obj      = best_state.objective_cost(demand);

        let accept = accept_candidate(cand_unmet, best_unmet, candidate_obj, best_obj);

        if accept {
            // Δobj < 0 всегда: критерий приёма требует строгого уменьшения цели.
            let delta_obj = candidate_obj - best_obj;
            best_state    = candidate.clone();
            current_state = candidate;

            stats.improvements        += 1;
            iters_no_improvement       = 0;

            // Адаптация K вниз: нашли улучшение — сужаем окрестность.
            destroy_ratio = (destroy_ratio - DESTROY_RATIO_STEP_DOWN)
                .max(DESTROY_RATIO_MIN);

            let (bu, be) = best_state.unmet_and_excess(demand);
            println!(
                "[iter {:>5}] ✓ Δobj {:+.2} | real {:.2} | penalty {:.2} | undist {} (unmet {} + excess {}) | K={:.0}%",
                stats.iterations,
                delta_obj,
                best_state.total_cost,
                best_state.penalty_component_cost(demand),
                bu + be,
                bu,
                be,
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

        // Периодический «heartbeat» — даже если давно не было accept,
        // пользователь видит, что цикл работает и куда движутся метрики
        // best_state (а не отдельного кандидата — они всегда монотонны).
        if stats.iterations % 100 == 0 {
            let (bu, be) = best_state.unmet_and_excess(demand);
            let elapsed_s = start.elapsed().as_secs_f64();
            let budget_s  = config.time_budget.as_secs_f64();
            println!(
                "[iter {:>5}] · {:>5.1}/{:.0}s | best real {:.2} | undist {} (unmet {} + excess {}) | accept {} | K={:.0}%",
                stats.iterations,
                elapsed_s, budget_s,
                best_state.total_cost,
                bu + be, bu, be,
                stats.improvements,
                destroy_ratio * 100.0,
            );
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

// Запускает полный пайплайн: жадное решение → ALNS.
//
// Используй эту функцию вместо прямого вызова `solve()` для крупных задач.
// pub fn solve_with_alns(
//     arcs:   &[TaskArc],
//     supply: &[SupplyNode],
//     demand: &[DemandNode],
//     config: &AlnsConfig,
// ) -> AlnsResult {
//     use super::greedy::{greedy_initial_solution, print_greedy_result};
//     use super::lp::print_balance;

//     print_balance(supply, demand);

//     let greedy = greedy_initial_solution(arcs, supply, demand);
//     print_greedy_result(&greedy, supply, demand);

//     run_alns(&greedy, arcs, supply, demand, config)
// }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{CarKind, RepairStatus};

    // --- Критерий приёма -------------------------------------------------

    /// Инцидент 07.09.2026: excess 2077 → 2075 (−10 000 руб. штрафа) при росте
    /// real_cost на 35 млн. Прежний лексикографический критерий принимал —
    /// новый обязан отклонить.
    #[test]
    fn accept_rejects_cost_blowup_for_small_excess_drop() {
        let best_obj = 257_974_231.57 + PENALTY_EXCESS * 2077.0;
        let cand_obj = 292_969_532.75 + PENALTY_EXCESS * 2075.0;
        assert!(!accept_candidate(0, 0, cand_obj, best_obj));
    }

    /// Закрыть заявку важнее, чем избавиться от вагона в остатке:
    /// unmet 5→4 при excess 20→21 принимается (−1 000 000 + 5 000 < 0).
    #[test]
    fn accept_takes_unmet_drop_even_if_excess_grows() {
        let real = 10_000_000.0;
        let best_obj = real + PENALTY_UNMET * 5.0 + PENALTY_EXCESS * 20.0;
        let cand_obj = real + PENALTY_UNMET * 4.0 + PENALTY_EXCESS * 21.0;
        assert!(accept_candidate(4, 5, cand_obj, best_obj));
    }

    /// Рост unmet запрещён даже при формально меньшей objective.
    #[test]
    fn accept_rejects_unmet_growth() {
        assert!(!accept_candidate(1, 0, 1_000.0, 5_000_000.0));
    }

    /// Требуется строгое улучшение не меньше ACCEPT_MIN_IMPROVEMENT_RUB.
    #[test]
    fn accept_requires_strict_improvement() {
        let best = 1_000_000.0;
        assert!(!accept_candidate(0, 0, best, best));
        assert!(!accept_candidate(0, 0, best - 0.4, best));
        assert!(accept_candidate(0, 0, best - ACCEPT_MIN_IMPROVEMENT_RUB, best));
        assert!(accept_candidate(0, 0, best - 50.0, best));
    }

    // --- Warm-start подзадачи --------------------------------------------

    fn sub_arc(id: usize, s: usize, d: usize) -> TaskArc {
        TaskArc {
            arc_id: id,
            s_idx: s,
            d_idx: d,
            supply_station_code: format!("S{s}"),
            demand_station_code: format!("D{d}"),
            cost: 1.0,
            distance: 1,
            delivery_days: 1,
            period_ok: true,
            car_type_ok: true,
            pair_min_batch: 0,
        }
    }

    fn assignment(arc_id: usize, s: usize, d: usize, qty: i32) -> Assignment {
        Assignment { arc_id, s_idx: s, d_idx: d, quantity: qty, total_cost: qty as f64 }
    }

    /// Удалённые назначения (в оригинальных индексах) ложатся на дуги подзадачи
    /// через s_map/d_map; несколько назначений одной пары суммируются; пары вне
    /// подзадачи игнорируются.
    #[test]
    fn warm_start_maps_removed_onto_sub_arcs() {
        // Локальные узлы: s 0→orig 7, 1→orig 9; d 0→orig 3, 1→orig 4.
        let s_map = vec![7, 9];
        let d_map = vec![3, 4];
        let sub_arcs = vec![
            sub_arc(0, 0, 0), // (7,3)
            sub_arc(1, 0, 1), // (7,4)
            sub_arc(2, 1, 1), // (9,4)
        ];
        let removed = vec![
            assignment(100, 7, 4, 3),
            assignment(101, 7, 4, 2),   // та же пара → суммируется
            assignment(102, 9, 4, 5),
            assignment(103, 9, 3, 1),   // пары (9,3) в подзадаче нет → игнор
        ];
        let warm = warm_start_from_removed(&removed, &sub_arcs, &s_map, &d_map);
        assert_eq!(warm, vec![0.0, 5.0, 5.0]);
    }

    // --- Интеграция: ALNS не ухудшает seed --------------------------------

    fn supply_node(idx: usize, count: i32) -> SupplyNode {
        SupplyNode {
            s_id: idx + 1,
            kind: CarKind::Free,
            car_count: count,
            station_to: String::new(),
            station_to_code: format!("S{idx}"),
            railway_to: String::new(),
            railway_to_code: None,
            railway_part_to: None,
            car_type: Some("Прочие".to_string()),
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

    fn demand_node(idx: usize, count: i32) -> DemandNode {
        DemandNode {
            d_id: idx + 1,
            purpose: DemandPurpose::Load,
            period: 1,
            station_name: String::new(),
            station_code: format!("D{idx}"),
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

    fn task_arc(id: usize, s: usize, d: usize, cost: f64) -> TaskArc {
        let mut a = sub_arc(id, s, d);
        a.cost = cost;
        a
    }

    fn seed_from(assignments: Vec<Assignment>, supply: &[SupplyNode], demand: &[DemandNode]) -> GreedyResult {
        let total_cost = assignments.iter().map(|a| a.total_cost).sum();
        let assigned: i32 = assignments.iter().map(|a| a.quantity).sum();
        let total_supply: i32 = supply.iter().map(|s| s.car_count).sum();
        let total_demand: i32 = demand.iter().map(|d| d.car_count).sum();
        GreedyResult {
            assignments,
            total_cost,
            assigned_cars: assigned,
            unmet_demand: total_demand - assigned,
            excess_supply: total_supply - assigned,
        }
    }

    fn quick_config() -> AlnsConfig {
        AlnsConfig {
            time_budget: Duration::from_millis(400),
            destroy_ratio: DESTROY_RATIO_INIT,
            seed: Some(7),
            use_mip_repair: true,
        }
    }

    /// Плохой seed (дорогая дуга, заявка D0 не закрыта, S1 в остатке): одно
    /// разрушение + MIP-ремонт с warm-start находят оптимум S0→D0, S1→D1.
    /// Финальная objective строго меньше стартовой.
    #[test]
    fn alns_improves_bad_seed_and_never_worsens() {
        let supply = vec![supply_node(0, 1), supply_node(1, 1)];
        let demand = vec![demand_node(0, 1), demand_node(1, 1)];
        let arcs = vec![
            task_arc(0, 0, 0, 10.0),
            task_arc(1, 0, 1, 1_000.0),
            task_arc(2, 1, 1, 10.0),
        ];
        let seed = seed_from(
            vec![Assignment { arc_id: 1, s_idx: 0, d_idx: 1, quantity: 1, total_cost: 1_000.0 }],
            &supply, &demand,
        );
        let seed_obj = seed.objective_cost();

        let res = run_alns(&seed, &arcs, &supply, &demand, &quick_config(), None);
        let best_obj = res.best_state.objective_cost(&demand);

        assert!(best_obj <= seed_obj, "ALNS не должен ухудшать seed: {best_obj} > {seed_obj}");
        let (unmet, excess) = res.best_state.unmet_and_excess(&demand);
        assert_eq!((unmet, excess), (0, 0));
        assert!((res.best_state.total_cost - 20.0).abs() < 1e-6, "ожидался оптимум 20, получено {}", res.best_state.total_cost);
    }

    /// Оптимальный seed: ни одна итерация не должна быть принята, состояние
    /// возвращается без изменений (регрессия на «принять любое снижение excess»).
    #[test]
    fn alns_keeps_optimal_seed_untouched() {
        let supply = vec![supply_node(0, 2), supply_node(1, 2), supply_node(2, 1)];
        let demand = vec![demand_node(0, 2), demand_node(1, 2)];
        let arcs = vec![
            task_arc(0, 0, 0, 10.0),
            task_arc(1, 0, 1, 500.0),
            task_arc(2, 1, 0, 500.0),
            task_arc(3, 1, 1, 10.0),
            task_arc(4, 2, 0, 400.0),
            task_arc(5, 2, 1, 400.0),
        ];
        // Оптимум: S0→D0 (2), S1→D1 (2); S2 в остатке (excess 1).
        let seed = seed_from(
            vec![
                Assignment { arc_id: 0, s_idx: 0, d_idx: 0, quantity: 2, total_cost: 20.0 },
                Assignment { arc_id: 3, s_idx: 1, d_idx: 1, quantity: 2, total_cost: 20.0 },
            ],
            &supply, &demand,
        );
        let seed_obj = seed.objective_cost();

        let res = run_alns(&seed, &arcs, &supply, &demand, &quick_config(), None);

        assert_eq!(res.stats.improvements, 0, "оптимальный seed не должен «улучшаться»");
        assert!((res.best_state.objective_cost(&demand) - seed_obj).abs() < 1e-6);
        assert!((res.best_state.total_cost - 40.0).abs() < 1e-6);
    }
}

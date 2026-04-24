//! MIP-постановка транспортной задачи с жёстким ограничением MIN_BATCH
//! на уровне пар станций массовой выгрузки.
//!
//! Реализует big-M формулировку дизъюнкции «поток по паре = 0 или ≥ MIN_BATCH»:
//! для каждой пары `(mass_supply_station, demand_station)` вводится бинарная
//! переменная `y_pair ∈ {0, 1}` и два линейных ограничения:
//!
//! ```text
//! B_pair * y_pair  ≤  Σ x[arc]               (достичь либо нуля, либо партии ≥ B_pair)
//! Σ x[arc]         ≤  M_pair * y_pair        (разрешить поток только при y_pair = 1)
//! ```
//!
//! где `B_pair = min(MIN_BATCH_FROM_MASS_STATION, supply_at_mass_station)`,
//! `M_pair = supply_at_mass_station` — тонкая верхняя оценка суммарного потока по паре
//! (улучшает LP-релаксацию и ускоряет branch-and-cut).
//!
//! Дуговые переменные целые (`add_integer_column`), `y_pair` — бинарные.
//! Dummy-узлы (избыток / неудовл. спрос) — непрерывные, их целостность гарантируется
//! целочисленностью дуг и integer-правой частью.
//!
//! Поддерживается warm-start из жадного решения: greedy даёт допустимое назначение
//! (inline-проверки MIN_BATCH гарантируют это), HiGHS принимает его как incumbent.

use std::collections::HashMap;
use std::time::Duration;

use highs::{ColProblem, HighsModelStatus, Row, Sense};

use super::greedy::{Assignment, GreedyResult};
use super::lp::{OptimResult, PENALTY_COST};
use super::model::{MIN_BATCH_FROM_MASS_STATION, TaskArc};
use crate::node::{DemandNode, DemandPurpose, SupplyNode};

// ---------------------------------------------------------------------------
// Константы
// ---------------------------------------------------------------------------

/// Бюджет времени MIP-решателя по умолчанию.
///
/// По истечении бюджета HiGHS возвращает лучшее найденное допустимое решение
/// (warm-start из жадного гарантированно задаёт нижнюю планку).
pub const DEFAULT_MIP_TIME_LIMIT: Duration = Duration::from_secs(120);

/// Целевой относительный разрыв MIP: при достижении HiGHS останавливается раньше лимита.
pub const DEFAULT_MIP_REL_GAP: f64 = 0.005;

// ---------------------------------------------------------------------------
// Результат MIP-решения
// ---------------------------------------------------------------------------

/// Полный результат решения MIP-подзадачи.
///
/// В отличие от [`OptimResult`], содержит сырой статус HiGHS и MIP-gap,
/// необходимые для принятия решений (пропуск ALNS, fallback в repair).
#[derive(Debug, Clone)]
pub struct MipOutcome {
    /// Сводный [`OptimResult`] (стоимость, покрытие, статус в человекочитаемом виде).
    pub optim: OptimResult,
    /// Значения дуговых переменных в порядке `arcs`.
    pub arc_vals: Vec<f64>,
    /// Сырой статус HiGHS: [`HighsModelStatus::Optimal`], [`HighsModelStatus::ReachedTimeLimit`], ...
    pub status: HighsModelStatus,
    /// Относительный MIP-gap. `0.0` при достижении глобального оптимума;
    /// `f64::INFINITY` если модель не содержит целочисленных переменных.
    pub mip_gap: f64,
}

impl MipOutcome {
    /// Решение признано глобальным оптимумом (gap ≈ 0 при `HighsModelStatus::Optimal`).
    ///
    /// При `Optimal` HiGHS гарантирует оптимальность в рамках `mip_rel_gap`; строгое
    /// `gap < 1e-6` фильтрует случаи, когда решатель остановился по допустимому разрыву.
    pub fn is_globally_optimal(&self) -> bool {
        self.status == HighsModelStatus::Optimal
            && self.mip_gap.is_finite()
            && self.mip_gap < 1e-6
    }

    /// Есть ли в распоряжении допустимое (может быть субоптимальное) решение.
    ///
    /// `true` для `Optimal`, `ReachedTimeLimit`, `ObjectiveBound`, `ObjectiveTarget`,
    /// `ReachedSolutionLimit` — во всех этих случаях HiGHS возвращает incumbent.
    pub fn has_feasible_solution(&self) -> bool {
        matches!(
            self.status,
            HighsModelStatus::Optimal
                | HighsModelStatus::ReachedTimeLimit
                | HighsModelStatus::ObjectiveBound
                | HighsModelStatus::ObjectiveTarget
                | HighsModelStatus::ReachedSolutionLimit
        )
    }
}

// ---------------------------------------------------------------------------
// Основная точка входа
// ---------------------------------------------------------------------------

/// Решает задачу как MIP с жёстким ограничением MIN_BATCH на парах станций массовой выгрузки.
///
/// `warm_start` — начальные значения для дуговых переменных (`Vec<f64>` длины `arcs.len()`,
/// обычно результат [`super::greedy::greedy_to_arc_vals`]). Значения для dummy- и бинарных
/// переменных достраиваются автоматически.
///
/// `rel_gap` — целевой относительный разрыв остановки; при `None` используется
/// [`DEFAULT_MIP_REL_GAP`].
///
/// Возвращает [`MipOutcome`] со статусом HiGHS, MIP-gap и значениями дуговых переменных
/// в порядке `arcs`.
pub fn solve_mip(
    arcs: &[TaskArc],
    supply: &[SupplyNode],
    demand: &[DemandNode],
    time_limit: Duration,
    warm_start: Option<&[f64]>,
    rel_gap: Option<f64>,
) -> MipOutcome {
    // -----------------------------------------------------------------------
    // 1. Сбор пар станций массовой выгрузки и суммарного предложения по ним.
    // -----------------------------------------------------------------------
    let mut mass_pair_arcs: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, arc) in arcs.iter().enumerate() {
        if arc.is_mass_unloading {
            mass_pair_arcs
                .entry((
                    arc.supply_station_code.clone(),
                    arc.demand_station_code.clone(),
                ))
                .or_default()
                .push(i);
        }
    }

    // Суммарное предложение на каждой станции массовой выгрузки (используется для
    // тонкой оценки M_pair и для клиппинга B_pair — см. аналогичный трюк в example.py,
    // `min(_ASSIGN_BULK_BOUND_, station_supply)`).
    let mut station_supply: HashMap<String, i32> = HashMap::new();
    for s in supply.iter().filter(|s| s.is_mass_unloading) {
        *station_supply.entry(s.station_to_code.clone()).or_insert(0) += s.car_count;
    }

    // Стабилизируем порядок пар — он определяет позиции бинарных столбцов
    // и, как следствие, правильность выравнивания warm-start по этим столбцам.
    let mut pair_list: Vec<((String, String), Vec<usize>)> = mass_pair_arcs.into_iter().collect();
    pair_list.sort_by(|a, b| a.0.cmp(&b.0));

    let total_supply: f64 = supply.iter().map(|s| s.car_count as f64).sum();
    let total_load_demand: f64 = demand
        .iter()
        .filter(|d| d.purpose == DemandPurpose::Load)
        .map(|d| d.car_count as f64)
        .sum();

    let n_arcs = arcs.len();
    let n_supply = supply.len();
    let n_load_demand = demand
        .iter()
        .filter(|d| d.purpose == DemandPurpose::Load)
        .count();
    let n_pairs = pair_list.len();

    // -----------------------------------------------------------------------
    // 2. Построение модели: сначала все строки, потом все столбцы.
    // -----------------------------------------------------------------------
    let mut model = ColProblem::default();

    // Строки предложения: Σ x[из s] + dummy_demand[s] = car_count[s]
    let supply_rows: Vec<Row> = supply
        .iter()
        .map(|s| {
            let c = s.car_count as f64;
            model.add_row(c..=c)
        })
        .collect();

    // Строки спроса: погрузка — равенство; промывка — верхняя ёмкость.
    let demand_rows: Vec<Row> = demand
        .iter()
        .map(|d| {
            let c = d.car_count as f64;
            if d.purpose == DemandPurpose::Wash {
                model.add_row(0.0..=c)
            } else {
                model.add_row(c..=c)
            }
        })
        .collect();

    // Dummy-узлы: поглощение избытка и штрафное покрытие неудовлетворённого спроса.
    let dummy_demand_row = model.add_row(0.0..=total_supply);
    let dummy_supply_row = model.add_row(0.0..=total_load_demand);

    // Парные строки MIN_BATCH:
    //   pair_lower: B*y - Σ x ≤ 0  ⇔  Σ x ≥ B*y
    //   pair_upper: Σ x - M*y ≤ 0  ⇔  Σ x ≤ M*y
    let mut pair_lower_rows: Vec<Row> = Vec::with_capacity(n_pairs);
    let mut pair_upper_rows: Vec<Row> = Vec::with_capacity(n_pairs);
    for _ in 0..n_pairs {
        pair_lower_rows.push(model.add_row(f64::NEG_INFINITY..=0.0));
        pair_upper_rows.push(model.add_row(f64::NEG_INFINITY..=0.0));
    }

    // Индекс: arc_id → индекс пары в pair_list (если арк участвует в паре).
    let mut arc_to_pair: Vec<Option<usize>> = vec![None; n_arcs];
    for (p_idx, (_, ids)) in pair_list.iter().enumerate() {
        for &aid in ids {
            arc_to_pair[aid] = Some(p_idx);
        }
    }

    // -----------------------------------------------------------------------
    // 3. Столбцы. Порядок: [arcs] [dummy-demand] [dummy-supply] [y_pair].
    //    Этот порядок совпадает с ожиданиями warm-start ниже.
    // -----------------------------------------------------------------------

    // Дуговые переменные — целочисленные, верхняя граница `min(supply, demand)`
    // даёт HiGHS полезную априорную информацию.
    for arc in arcs {
        let upper = supply[arc.s_idx].car_count.min(demand[arc.d_idx].car_count) as f64;
        let mut factors: Vec<(Row, f64)> = Vec::with_capacity(4);
        factors.push((supply_rows[arc.s_idx], 1.0));
        factors.push((demand_rows[arc.d_idx], 1.0));
        if let Some(p) = arc_to_pair[arc.arc_id] {
            factors.push((pair_lower_rows[p], -1.0));
            factors.push((pair_upper_rows[p], 1.0));
        }
        model.add_integer_column(arc.cost, 0.0..=upper, factors);
    }

    // Dummy-demand (поглощает избыток предложения, стоимость 0).
    for s_row in &supply_rows {
        model.add_column(0.0, 0.0.., [(*s_row, 1.0), (dummy_demand_row, 1.0)]);
    }

    // Dummy-supply (штрафное покрытие только для Load-спроса).
    for (d_row, d) in demand_rows.iter().zip(demand.iter()) {
        if d.purpose == DemandPurpose::Load {
            model.add_column(
                PENALTY_COST,
                0.0..,
                [(dummy_supply_row, 1.0), (*d_row, 1.0)],
            );
        }
    }

    // Бинарные y_pair ∈ {0,1} с двумя ограничениями: B*y ≤ Σx, Σx ≤ M*y.
    for (p_idx, ((ss, _ds), _)) in pair_list.iter().enumerate() {
        let station_sup = *station_supply.get(ss).unwrap_or(&0);
        let b = MIN_BATCH_FROM_MASS_STATION.min(station_sup) as f64;
        let m = station_sup as f64;
        model.add_integer_column(
            0.0,
            0.0..=1.0,
            [
                (pair_lower_rows[p_idx], b),
                (pair_upper_rows[p_idx], -m),
            ],
        );
    }

    // -----------------------------------------------------------------------
    // 4. Настройка решателя и warm-start.
    // -----------------------------------------------------------------------
    let mut solver = model.optimise(Sense::Minimise);
    solver.set_option("time_limit", time_limit.as_secs_f64());
    solver.set_option("mip_rel_gap", rel_gap.unwrap_or(DEFAULT_MIP_REL_GAP));
    solver.set_option("presolve", "on");
    solver.set_option("parallel", "on");
    solver.set_option("threads", 8_i32);

    if let Some(warm) = warm_start {
        if warm.len() == n_arcs {
            let total_cols = n_arcs + n_supply + n_load_demand + n_pairs;
            let mut cols_init: Vec<f64> = Vec::with_capacity(total_cols);

            // Arcs: greedy-values как есть.
            cols_init.extend_from_slice(warm);

            // Dummy-demand: избыток на каждом узле предложения после greedy.
            let mut supply_sent = vec![0.0_f64; n_supply];
            let mut demand_recv = vec![0.0_f64; demand.len()];
            for (arc, &q) in arcs.iter().zip(warm.iter()) {
                supply_sent[arc.s_idx] += q;
                demand_recv[arc.d_idx] += q;
            }
            for (i, s) in supply.iter().enumerate() {
                cols_init.push((s.car_count as f64 - supply_sent[i]).max(0.0));
            }

            // Dummy-supply: дефицит на каждом Load-узле после greedy.
            for (i, d) in demand.iter().enumerate() {
                if d.purpose == DemandPurpose::Load {
                    cols_init.push((d.car_count as f64 - demand_recv[i]).max(0.0));
                }
            }

            // y_pair: 1 если greedy сделал хотя бы одно назначение по паре.
            for (_, ids) in &pair_list {
                let flow: f64 = ids.iter().map(|&i| warm[i]).sum();
                cols_init.push(if flow > 1e-6 { 1.0 } else { 0.0 });
            }

            debug_assert_eq!(cols_init.len(), total_cols);

            if let Err(e) = solver.try_set_solution(Some(&cols_init), None, None, None) {
                eprintln!("  MIP warm-start отвергнут: {:?}", e);
            }
        } else {
            eprintln!(
                "  MIP warm-start пропущен: длина {} != arcs.len() {}",
                warm.len(),
                n_arcs
            );
        }
    }

    // -----------------------------------------------------------------------
    // 5. Решение и извлечение результата.
    // -----------------------------------------------------------------------
    let solved = solver.solve();
    let status = solved.status();
    let solution = solved.get_solution();
    let col_vals = solution.columns();

    let arc_vals = &col_vals[..n_arcs];
    let dummy_demand_vals = &col_vals[n_arcs..n_arcs + n_supply];
    let dummy_supply_vals = &col_vals[n_arcs + n_supply..n_arcs + n_supply + n_load_demand];

    let total_cost: f64 = arcs
        .iter()
        .zip(arc_vals)
        .filter(|(_, q)| **q > 1e-4)
        .map(|(a, &q)| q * a.cost)
        .sum();
    let assigned_cars: f64 = arc_vals.iter().filter(|&&q| q > 1e-4).sum();
    let excess_supply: f64 = dummy_demand_vals.iter().filter(|&&q| q > 1e-4).sum();
    let penalty_cars: f64 = dummy_supply_vals.iter().filter(|&&q| q > 1e-4).sum();

    let gap = solved.mip_gap();
    let status_str = if gap.is_finite() {
        format!("MIP {:?} (gap={:.3}%)", status, gap * 100.0)
    } else {
        format!("MIP {:?}", status)
    };

    let result = OptimResult {
        total_cost,
        assigned_cars,
        penalty_cars,
        excess_supply,
        status: status_str,
    };

    MipOutcome {
        optim: result,
        arc_vals: arc_vals.to_vec(),
        status,
        mip_gap: gap,
    }
}

// ---------------------------------------------------------------------------
// Вспомогательные функции
// ---------------------------------------------------------------------------

/// Конвертирует значения дуговых переменных в [`GreedyResult`], пригодный для
/// [`super::alns::AlnsState::from_greedy`]. Значения округляются к ближайшему целому.
///
/// Используется для передачи решения MIP в качестве стартового состояния ALNS.
pub fn arc_vals_to_greedy_result(
    arc_vals: &[f64],
    arcs: &[TaskArc],
    supply: &[SupplyNode],
    demand: &[DemandNode],
) -> GreedyResult {
    let mut assignments: Vec<Assignment> = Vec::new();
    let mut total_cost: f64 = 0.0;
    let mut assigned_cars: i32 = 0;
    let mut sent = vec![0_i32; supply.len()];
    let mut recv = vec![0_i32; demand.len()];

    for (arc, &q) in arcs.iter().zip(arc_vals.iter()) {
        let qty = q.round() as i32;
        if qty <= 0 {
            continue;
        }
        let cost = qty as f64 * arc.cost;
        assignments.push(Assignment {
            arc_id: arc.arc_id,
            s_idx: arc.s_idx,
            d_idx: arc.d_idx,
            quantity: qty,
            total_cost: cost,
        });
        total_cost += cost;
        assigned_cars += qty;
        sent[arc.s_idx] += qty;
        recv[arc.d_idx] += qty;
    }

    let unmet_demand: i32 = demand
        .iter()
        .enumerate()
        .filter(|(_, d)| d.purpose == DemandPurpose::Load)
        .map(|(i, d)| (d.car_count - recv[i]).max(0))
        .sum();
    let excess_supply: i32 = supply
        .iter()
        .enumerate()
        .map(|(i, s)| (s.car_count - sent[i]).max(0))
        .sum();

    GreedyResult {
        assignments,
        total_cost,
        assigned_cars,
        unmet_demand,
        excess_supply,
    }
}

/// Выводит сводку MIP-решения в консоль.
pub fn print_mip_result(result: &OptimResult, supply: &[SupplyNode], demand: &[DemandNode]) {
    let total_supply: i32 = supply.iter().map(|s| s.car_count).sum();
    let total_load_demand: i32 = demand
        .iter()
        .filter(|d| d.purpose == DemandPurpose::Load)
        .map(|d| d.car_count)
        .sum();

    println!("--- MIP РЕШЕНИЕ ---");
    println!("Статус:                {}", result.status);
    println!(
        "Назначено вагонов:     {:.0} / {} спрос (погрузка), {} предложение",
        result.assigned_cars, total_load_demand, total_supply
    );
    println!("Суммарная стоимость:   {:.2} руб.", result.total_cost);
    println!("Неудовлетворён спрос:  {:.0} ваг.", result.penalty_cars);
    println!("Избыток предложения:   {:.0} ваг.", result.excess_supply);
    if total_load_demand > 0 {
        println!(
            "Покрытие спроса (погр.): {:.1}%",
            result.assigned_cars / total_load_demand as f64 * 100.0
        );
    }
    println!("-------------------");
}

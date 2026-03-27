use highs::{ColProblem, Sense};
use serde::Serialize;

use crate::node::{DemandNode, SupplyNode};
use super::model::TaskArc;

// ---------------------------------------------------------------------------
// Результат оптимизации
// ---------------------------------------------------------------------------

/// Сводная статистика после решения LP-задачи.
#[derive(Debug, Clone, Serialize)]
pub struct OptimResult {
    /// Суммарная стоимость по реальным дугам.
    pub total_cost: f64,
    /// Вагоны, успешно назначенные на реальные узлы спроса.
    pub assigned_cars: f64,
    /// Вагоны, назначенные на dummy-узел предложения (неудовлетворённый спрос).
    pub penalty_cars: f64,
    /// Вагоны, назначенные на dummy-узел спроса (избыток предложения, не нашедший погрузки).
    pub excess_supply: f64,
    /// Статус решателя (строка из HiGHS).
    pub status: String,
}

// ---------------------------------------------------------------------------
// LP-решатель
// ---------------------------------------------------------------------------

/// Штраф за 1 вагон неудовлетворённого спроса (руб.).
///
/// Выше любого реального тарифа — решатель предпочитает реальные дуги.
/// Конечный — задача остаётся разрешимой при дефиците предложения.
const PENALTY_COST: f64 = 10_000_000.0;

/// Решает сбалансированную транспортную задачу методом LP (HiGHS / IPM).
///
/// # Балансировка через dummy-узлы
///
/// Перед решением задача балансируется: добавляется ровно один dummy-узел,
/// чтобы суммарное предложение точно равнялось суммарному спросу.
///
/// | Ситуация          | Действие                                                    |
/// |-------------------|-------------------------------------------------------------|
/// | supply > demand   | Dummy-узел **спроса**: поглощает излишек по стоимости 0    |
/// | demand > supply   | Dummy-узел **предложения**: покрывает дефицит по PENALTY   |
/// | supply == demand  | Dummy-узел не добавляется                                   |
///
/// После балансировки ограничения предложения/спроса становятся **равенствами**,
/// и LP решается строже и быстрее.
///
/// # Возврат
/// `(OptimResult, Vec<f64>)` — второй элемент содержит значения только
/// **реальных дуговых** переменных (в порядке `arcs`), без dummy.
pub fn solve(
    arcs: &[TaskArc],
    supply: &[SupplyNode],
    demand: &[DemandNode],
) -> (OptimResult, Vec<f64>) {
    let total_supply: i32 = supply.iter().map(|s| s.car_count).sum();
    let total_demand: i32 = demand.iter().map(|d| d.car_count).sum();
    let diff = total_supply - total_demand; // >0 профицит, <0 дефицит

    let mut model = ColProblem::default();

    // --- Строки ограничений (равенства после добавления dummy) ---

    // Предложение: Σ x[из s] = car_count[s]
    let supply_rows: Vec<_> = supply
        .iter()
        .map(|s| {
            let c = s.car_count as f64;
            model.add_row(c..=c)
        })
        .collect();

    // Спрос: Σ x[в d] = car_count[d]
    let demand_rows: Vec<_> = demand
        .iter()
        .map(|d| {
            let c = d.car_count as f64;
            model.add_row(c..=c)
        })
        .collect();

    // --- Реальные дуговые переменные ---
    for arc in arcs {
        model.add_column(
            arc.cost,
            0.0..,
            [(supply_rows[arc.s_idx], 1.0), (demand_rows[arc.d_idx], 1.0)],
        );
    }

    // --- Dummy-узел для балансировки ---
    let (n_dummy, dummy_is_demand) = if diff > 0 {
        // Профицит: dummy-узел СПРОСА поглощает excess supply по стоимости 0.
        // Каждый узел предложения получает дугу в dummy-demand.
        let dummy_row = model.add_row(diff as f64..=diff as f64);
        for s_row in &supply_rows {
            model.add_column(0.0, 0.0.., [(*s_row, 1.0), (dummy_row, 1.0)]);
        }
        (supply.len(), true)
    } else if diff < 0 {
        // Дефицит: dummy-узел ПРЕДЛОЖЕНИЯ покрывает нехватку по штрафу.
        // Каждый узел спроса получает дугу из dummy-supply.
        let dummy_row = model.add_row((-diff) as f64..=(-diff) as f64);
        for d_row in &demand_rows {
            model.add_column(PENALTY_COST, 0.0.., [(dummy_row, 1.0), (*d_row, 1.0)]);
        }
        (demand.len(), false)
    } else {
        (0, false)
    };

    // --- Запуск решателя ---
    // IPM значительно быстрее simplex для задач с >50K переменных.
    let mut optimizer = model.optimise(Sense::Minimise);
    optimizer.set_option("solver",   "ipm");
    optimizer.set_option("presolve", "on");
    optimizer.set_option("parallel", "on");
    optimizer.set_option("threads",  8_i32);

    let solved    = optimizer.solve();
    let solution  = solved.get_solution();
    let col_vals  = solution.columns();

    // Столбцы: [реальные дуги (n_arcs)] + [dummy дуги (n_dummy)]
    let n_arcs     = arcs.len();
    let arc_vals   = &col_vals[..n_arcs];
    let dummy_vals = &col_vals[n_arcs..n_arcs + n_dummy];

    let dummy_total: f64 = dummy_vals.iter().filter(|&&q| q > 1e-4).sum();

    // --- Сбор статистики ---
    let total_cost: f64 = arcs.iter().zip(arc_vals)
        .filter(|(_, q)| **q > 1e-4)
        .map(|(a, &q)| q * a.cost)
        .sum();

    let assigned_cars: f64 = arc_vals.iter().filter(|&&q| q > 1e-4).sum();

    let (penalty_cars, excess_supply) = if dummy_is_demand {
        (0.0, dummy_total)   // профицит → лишние вагоны уходят в dummy-demand
    } else {
        (dummy_total, 0.0)   // дефицит → нехватка покрывается dummy-supply
    };

    let result = OptimResult {
        total_cost,
        assigned_cars,
        penalty_cars,
        excess_supply,
        status: format!("{:?}", solved.status()),
    };

    (result, arc_vals.to_vec())
}

// ---------------------------------------------------------------------------
// Анализ баланса (вывод до solve)
// ---------------------------------------------------------------------------

/// Выводит в консоль соотношение суммарного предложения и спроса.
pub fn print_balance(supply: &[SupplyNode], demand: &[DemandNode]) {
    let total_supply: i32 = supply.iter().map(|s| s.car_count).sum();
    let total_demand: i32 = demand.iter().map(|d| d.car_count).sum();
    let diff = total_supply - total_demand;

    println!("--- АНАЛИЗ РЕСУРСОВ ---");
    println!("Предложение: {} ваг.", total_supply);
    println!("Спрос:       {} ваг.", total_demand);
    if diff >= 0 {
        println!("Статус: ПРОФИЦИТ (+{} ваг.)", diff);
    } else {
        println!("Статус: ДЕФИЦИТ  ({} ваг. — штрафные дуги)", diff.abs());
    }
    println!("-----------------------");
}

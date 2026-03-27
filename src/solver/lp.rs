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
    /// Суммарная стоимость по дугам с реальными тарифами (без штрафных).
    pub total_cost: f64,
    /// Количество назначенных вагонов по тарифным дугам.
    pub assigned_cars: f64,
    /// Количество вагонов, назначенных по штрафным дугам (нет тарифа / нет периода).
    pub penalty_cars: f64,
    /// Статус решателя (строка из HiGHS).
    pub status: String,
}

// ---------------------------------------------------------------------------
// LP-решатель
// ---------------------------------------------------------------------------

/// Стоимость штрафной дуги (руб.): значительно выше любого реального тарифа,
/// но конечное — чтобы задача оставалась разрешимой при дефиците предложения.
const PENALTY_COST: f64 = 10_000_000.0;

/// Решает транспортную задачу методом LP (HiGHS / simplex).
///
/// # Модель
///
/// **Переменные:** `x[arc_id]` ≥ 0 — кол-во вагонов на дуге.
///
/// **Целевая функция:** min Σ cost[i] · x[i]
///
/// **Ограничения:**
/// - Для каждого узла предложения s:  Σ x[i | arc.s_idx==s] ≤ supply[s].car_count
/// - Для каждого узла спроса d:       Σ x[i | arc.d_idx==d] ≥ demand[d].car_count
///
/// Возвращает `(OptimResult, Vec<f64>)`, где второй элемент — вектор значений
/// переменных в том же порядке, что и `arcs`.
pub fn solve(
    arcs: &[TaskArc],
    supply: &[SupplyNode],
    demand: &[DemandNode],
) -> (OptimResult, Vec<f64>) {
    let mut model = ColProblem::default();

    // --- Строки ограничений ---

    // Ограничения предложения: Σ x[arcs из s] ≤ car_count[s]  (верхняя граница)
    let supply_rows: Vec<_> = supply
        .iter()
        .map(|s| model.add_row(..s.car_count as f64))
        .collect();

    // Ограничения спроса: Σ x[arcs в d] ≥ car_count[d]  (нижняя граница)
    let demand_rows: Vec<_> = demand
        .iter()
        .map(|d| model.add_row(d.car_count as f64..))
        .collect();

    // --- Переменные (колонки) ---
    for arc in arcs {
        let cost = if arc.period_ok && arc.car_type_ok {
            arc.cost
        } else {
            PENALTY_COST
        };

        model.add_column(
            cost,
            0.0..,
            [
                (supply_rows[arc.s_idx], 1.0),
                (demand_rows[arc.d_idx], 1.0),
            ],
        );
    }

    // --- Запуск решателя ---
    let mut optimizer = model.optimise(Sense::Minimise);
    optimizer.set_option("solver",   "simplex");
    optimizer.set_option("presolve", "on");
    optimizer.set_option("parallel", "on");
    optimizer.set_option("threads",  8_i32);

    let solved   = optimizer.solve();
    let solution = solved.get_solution();
    let col_vals = solution.columns();

    // --- Сбор статистики ---
    let mut total_cost    = 0.0_f64;
    let mut assigned_cars = 0.0_f64;
    let mut penalty_cars  = 0.0_f64;

    for (arc, &qty) in arcs.iter().zip(col_vals.iter()) {
        if qty > 1e-4 {
            if arc.period_ok && arc.car_type_ok {
                total_cost    += qty * arc.cost;
                assigned_cars += qty;
            } else {
                penalty_cars += qty;
            }
        }
    }

    let result = OptimResult {
        total_cost,
        assigned_cars,
        penalty_cars,
        status: format!("{:?}", solved.status()),
    };

    (result, col_vals.to_vec())
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

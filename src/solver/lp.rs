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
pub const PENALTY_COST: f64 = 1_000_000.0;

/// Решает сбалансированную транспортную задачу методом LP (HiGHS / IPM).
///
/// # Балансировка через явные dummy-узлы
///
/// Задача всегда сбалансирована: суммарное предложение = суммарный спрос.
/// Достигается добавлением двух dummy-узлов с дугами ко/от **всех** реальных узлов:
///
/// | Dummy-узел        | Ёмкость         | Стоимость дуги | Назначение                       |
/// |-------------------|-----------------|----------------|----------------------------------|
/// | Dummy **спрос**   | `total_supply`  | 0              | Поглощает незадействованное предложение |
/// | Dummy **предложение** | `total_demand` | `PENALTY`  | Покрывает незакрытый спрос       |
///
/// Оба узла имеют ёмкость, равную **полному** предложению/спросу, а не дефициту.
/// Это гарантирует разрешимость даже если отдельный узел спроса полностью
/// изолирован (нет реальных дуг): он всегда покрывается dummy-предложением.
///
/// После балансировки все ограничения — **равенства**, LP решается строже и быстрее.
///
/// # Возврат
/// `(OptimResult, Vec<f64>)` — второй элемент содержит значения только
/// **реальных дуговых** переменных (в порядке `arcs`), без dummy.
pub fn solve(
    arcs: &[TaskArc],
    supply: &[SupplyNode],
    demand: &[DemandNode],
) -> (OptimResult, Vec<f64>) {
    let total_supply: f64 = supply.iter().map(|s| s.car_count as f64).sum();
    let total_demand: f64 = demand.iter().map(|d| d.car_count as f64).sum();

    let mut model = ColProblem::default();

    // --- Строки предложения: Σ x[из s] = car_count[s] ---
    let supply_rows: Vec<_> = supply
        .iter()
        .map(|s| { let c = s.car_count as f64; model.add_row(c..=c) })
        // .map(|s| { let c = s.car_count as f64; model.add_row(0.0..c) })
        .collect();

    // --- Строки спроса: Σ x[в d] = car_count[d] ---
    let demand_rows: Vec<_> = demand
        .iter()
        .map(|d| { let c = d.car_count as f64; model.add_row(c..=c) })
        // .map(|s| { let c = s.car_count as f64; model.add_row(c..) })
        .collect();

    // --- Реальные дуговые переменные ---
    for arc in arcs {
        model.add_column(
            arc.cost,
            0.0..,
            [(supply_rows[arc.s_idx], 1.0), (demand_rows[arc.d_idx], 1.0)],
        );
    }

    // --- Dummy-узел СПРОСА (поглощает незадействованное предложение) ---
    // Верхняя граница ≤ total_supply: может поглотить до всего предложения, но не обязан.
    // Стоимость дуг = 0: незадействованные вагоны не штрафуются.
    let dummy_demand_row = model.add_row(..total_supply);
    for s_row in &supply_rows {
        // model.add_column(0.0, 0.0.., [(*s_row, 1.0), (dummy_demand_row, 1.0)]);
        model.add_column(PENALTY_COST, 0.0.., [(*s_row, 1.0), (dummy_demand_row, 1.0)]);
    }

    // --- Dummy-узел ПРЕДЛОЖЕНИЯ (покрывает незакрытый спрос) ---
    // Верхняя граница ≤ total_demand: может покрыть до всего спроса, но не обязан.
    // Стоимость дуг = PENALTY: решатель предпочитает реальные дуги.
    let dummy_supply_row = model.add_row(..total_demand);
    for d_row in &demand_rows {
        model.add_column(PENALTY_COST, 0.0.., [(dummy_supply_row, 1.0), (*d_row, 1.0)]);
    }

    // --- Решатель ---
    // IPM значительно быстрее simplex для задач с >50K переменных.
    let mut optimizer = model.optimise(Sense::Minimise);
    optimizer.set_option("solver",   "simplex");
    optimizer.set_option("presolve", "on");
    optimizer.set_option("parallel", "on");
    optimizer.set_option("threads",  8_i32);

    let solved   = optimizer.solve();
    let solution = solved.get_solution();
    let col_vals = solution.columns();

    // Столбцы по порядку добавления:
    // [реальные дуги (n_arcs)] [dummy-demand дуги (n_supply)] [dummy-supply дуги (n_demand)]
    let n_arcs    = arcs.len();
    let n_supply  = supply.len();
    let n_demand  = demand.len();

    let arc_vals          = &col_vals[..n_arcs];
    let dummy_demand_vals = &col_vals[n_arcs..n_arcs + n_supply];
    let dummy_supply_vals = &col_vals[n_arcs + n_supply..n_arcs + n_supply + n_demand];

    // --- Статистика ---
    let total_cost: f64 = arcs.iter().zip(arc_vals)
        .filter(|(_, q)| **q > 1e-4)
        .map(|(a, &q)| q * a.cost)
        .sum();

    let assigned_cars: f64 = arc_vals.iter().filter(|&&q| q > 1e-4).sum();
    let excess_supply: f64 = dummy_demand_vals.iter().filter(|&&q| q > 1e-4).sum();
    let penalty_cars:  f64 = dummy_supply_vals.iter().filter(|&&q| q > 1e-4).sum();

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

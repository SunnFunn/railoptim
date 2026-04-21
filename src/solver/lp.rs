use highs::{ColProblem, Sense};
use serde::Serialize;

use crate::node::{DemandNode, DemandPurpose, SupplyNode};
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
pub const PENALTY_COST: f64 = 400_000.0;

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
/// | Dummy **предложение** | `total_load_demand` | `PENALTY` | Покрывает незакрытый спрос на погрузку |
///
/// Ёмкость dummy-узлов согласована с суммарным предложением / спросом на **погрузку**.
/// Узлы промывки — только верхняя граница входящего потока (без штрафа за незаполнение);
/// штрафные дуги dummy-предложения ведут только к узлам погрузки.
///
/// Строки спроса на погрузку — равенства; на промывку — неравенство «не больше ёмкости».
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
    let total_load_demand: f64 = demand
        .iter()
        .filter(|d| d.purpose == DemandPurpose::Load)
        .map(|d| d.car_count as f64)
        .sum();

    let mut model = ColProblem::default();

    // --- Строки предложения: Σ x[из s] = car_count[s] ---
    let supply_rows: Vec<_> = supply
        .iter()
        .map(|s| { let c = s.car_count as f64; model.add_row(c..=c) })
        // .map(|s| { let c = s.car_count as f64; model.add_row(0.0..c) })
        .collect();

    // --- Строки спроса: погрузка — равенство; промывка — только верхняя ёмкость (без штрафа за незаполнение).
    let demand_rows: Vec<_> = demand
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
        model.add_column(0.0, 0.0.., [(*s_row, 1.0), (dummy_demand_row, 1.0)]);
        // model.add_column(PENALTY_COST, 0.0.., [(*s_row, 1.0), (dummy_demand_row, 1.0)]);
    }

    // --- Dummy-узел ПРЕДЛОЖЕНИЯ (покрывает незакрытый спрос **погрузки**) ---
    // Штрафные дуги только к узлам спроса на погрузку; промывка — опциональный приёмник.
    let dummy_supply_row = model.add_row(..total_load_demand);
    for (d_row, d) in demand_rows.iter().zip(demand.iter()) {
        if d.purpose == DemandPurpose::Load {
            model.add_column(PENALTY_COST, 0.0.., [(dummy_supply_row, 1.0), (*d_row, 1.0)]);
        }
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
    // [реальные дуги (n_arcs)] [dummy-demand дуги (n_supply)] [dummy-supply только Load (n_load)]
    let n_arcs    = arcs.len();
    let n_supply  = supply.len();
    let n_load_demand = demand.iter().filter(|d| d.purpose == DemandPurpose::Load).count();

    let arc_vals          = &col_vals[..n_arcs];
    let dummy_demand_vals = &col_vals[n_arcs..n_arcs + n_supply];
    let dummy_supply_vals = &col_vals[n_arcs + n_supply..n_arcs + n_supply + n_load_demand];

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
///
/// В «спросе» учитывается только **погрузка**; ёмкость промывки выводится отдельно.
pub fn print_balance(supply: &[SupplyNode], demand: &[DemandNode]) {
    let total_supply: i32 = supply.iter().map(|s| s.car_count).sum();
    let total_load: i32 = demand
        .iter()
        .filter(|d| d.purpose == DemandPurpose::Load)
        .map(|d| d.car_count)
        .sum();
    let wash_cap: i32 = demand
        .iter()
        .filter(|d| d.purpose == DemandPurpose::Wash)
        .map(|d| d.car_count)
        .sum();
    let diff = total_supply - total_load;

    println!("--- АНАЛИЗ РЕСУРСОВ ---");
    println!("Предложение: {} ваг.", total_supply);
    println!("Спрос (погрузка): {} ваг.", total_load);
    if wash_cap > 0 {
        println!("Ёмкость промывки (верх): {} ваг.", wash_cap);
    }
    if diff >= 0 {
        println!("Статус: ПРОФИЦИТ к погрузке (+{} ваг.)", diff);
    } else {
        println!("Статус: ДЕФИЦИТ  ({} ваг. — штрафные дуги)", diff.abs());
    }
    println!("-----------------------");
}

use crate::node::{DemandNode, SupplyNode};
use super::model::{TaskArc, MIN_BATCH_FROM_MASS_STATION};

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
// Жадный алгоритм
// ---------------------------------------------------------------------------

/// Строит начальное допустимое решение жадным методом.
///
/// # Стратегия
///
/// 1. Отбрасываем дуги с `period_ok == false` или `car_type_ok == false` —
///    они физически недопустимы.
/// 2. Сортируем допустимые дуги по **стоимости** (возрастание).
///    Внутри одинаковой стоимости — по `distance` (ближе лучше).
/// 3. Проходим по отсортированным дугам. Для каждой дуги:
///    - Проверяем остатки предложения и спроса.
///    - Назначаем `min(remaining_supply[s], remaining_demand[d])` вагонов.
///    - Обновляем остатки.
///    - Прекращаем, когда весь спрос закрыт или предложение исчерпано.
///
/// # Почему это хорошая отправная точка для ALNS
///
/// - Гарантированно допустимо (только `period_ok && car_type_ok` дуги).
/// - Жадная сортировка по стоимости даёт решение, близкое к LP-оптимуму,
///   но без дробных значений.
/// - Быстро: O(|arcs| log |arcs|) — миллисекунды даже для 800K дуг.
///
/// # Параметры
/// - `arcs`   — плоский список всех дуг задачи.
/// - `supply` — узлы предложения (порядок совпадает с `s_idx` в дугах).
/// - `demand` — узлы спроса (порядок совпадает с `d_idx` в дугах).
pub fn greedy_initial_solution(
    arcs:   &[TaskArc],
    supply: &[SupplyNode],
    demand: &[DemandNode],
) -> GreedyResult {
    // --- Остатки предложения и спроса (мутабельные копии) ---
    let mut remaining_supply: Vec<i32> = supply.iter().map(|s| s.car_count).collect();
    let mut remaining_demand: Vec<i32> = demand.iter().map(|d| d.car_count).collect();

    // --- Фильтрация и сортировка допустимых дуг ---
    //
    // Используем индексы, чтобы не клонировать дуги.
    // Сортировка: (cost ASC, distance ASC).
    let mut feasible_arc_indices: Vec<usize> = arcs
        .iter()
        .enumerate()
        .filter(|(_, arc)| arc.period_ok && arc.car_type_ok)
        .map(|(i, _)| i)
        .collect();

    feasible_arc_indices.sort_unstable_by(|&a, &b| {
        let arc_a = &arcs[a];
        let arc_b = &arcs[b];
        arc_a.cost
            .partial_cmp(&arc_b.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| arc_a.distance.cmp(&arc_b.distance))
    });

    // --- Жадное назначение ---
    let mut assignments: Vec<Assignment> = Vec::new();
    let mut total_cost:    f64 = 0.0;
    let mut assigned_cars: i32 = 0;

    for arc_i in feasible_arc_indices {
        let arc = &arcs[arc_i];

        let avail_supply = remaining_supply[arc.s_idx];
        let avail_demand = remaining_demand[arc.d_idx];

        // Оба узла должны иметь остаток.
        if avail_supply <= 0 || avail_demand <= 0 {
            continue;
        }

        let qty = avail_supply.min(avail_demand);

        // Ограничение партии для станций массовой выгрузки:
        // допустимо только 0 или >= MIN_BATCH_FROM_MASS_STATION.
        if arc.is_mass_unloading && qty < MIN_BATCH_FROM_MASS_STATION {
            continue;
        }

        remaining_supply[arc.s_idx] -= qty;
        remaining_demand[arc.d_idx] -= qty;

        let arc_cost = qty as f64 * arc.cost;
        total_cost    += arc_cost;
        assigned_cars += qty;

        assignments.push(Assignment {
            arc_id:     arc.arc_id,
            s_idx:      arc.s_idx,
            d_idx:      arc.d_idx,
            quantity:   qty,
            total_cost: arc_cost,
        });

        // Ранний выход: весь спрос закрыт.
        if remaining_demand.iter().all(|&d| d <= 0) {
            break;
        }
    }

    // --- Итоговая статистика ---
    let unmet_demand:  i32 = remaining_demand.iter().filter(|&&d| d > 0).sum();
    let excess_supply: i32 = remaining_supply.iter().filter(|&&s| s > 0).sum();

    GreedyResult {
        assignments,
        total_cost,
        assigned_cars,
        unmet_demand,
        excess_supply,
    }
}

// ---------------------------------------------------------------------------
// Конвертация жадного решения в формат LP (Vec<f64> по arc_id)
// ---------------------------------------------------------------------------

/// Переводит `GreedyResult` в плоский вектор значений переменных LP,
/// совместимый с форматом `arc_vals` из `solve()`.
///
/// Индекс в векторе = `arc.arc_id`. Значение = количество назначенных вагонов.
/// Дуги без назначения получают 0.0.
pub fn greedy_to_arc_vals(result: &GreedyResult, n_arcs: usize) -> Vec<f64> {
    let mut arc_vals = vec![0.0_f64; n_arcs];
    for assignment in &result.assignments {
        arc_vals[assignment.arc_id] = assignment.quantity as f64;
    }
    arc_vals
}

// ---------------------------------------------------------------------------
// Диагностика
// ---------------------------------------------------------------------------

/// Выводит сводку жадного решения в консоль.
pub fn print_greedy_result(result: &GreedyResult, supply: &[SupplyNode], demand: &[DemandNode]) {
    let total_supply: i32 = supply.iter().map(|s| s.car_count).sum();
    let total_demand: i32 = demand.iter().map(|d| d.car_count).sum();

    println!("--- ЖАДНОЕ РЕШЕНИЕ ---");
    println!("Назначений:            {} шт.", result.assignments.len());
    println!("Назначено вагонов:     {} / {} спрос, {} предложение",
        result.assigned_cars, total_demand, total_supply);
    println!("Суммарная стоимость:   {:.2} руб.", result.total_cost);
    println!("Неудовлетворён спрос:  {} ваг.", result.unmet_demand);
    println!("Избыток предложения:   {} ваг.", result.excess_supply);
    println!("Покрытие спроса:       {:.1}%",
        result.assigned_cars as f64 / total_demand as f64 * 100.0);
    println!("----------------------");
}

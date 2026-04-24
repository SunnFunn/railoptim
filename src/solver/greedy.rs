use std::collections::{HashMap, HashSet};

use crate::node::{DemandNode, DemandPurpose, SupplyNode};
use super::model::{MIN_BATCH_FROM_MASS_STATION, TaskArc};

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
/// 1. Отбрасываем дуги с `car_type_ok == false`. Нарушение срока подсыла для части предложений
///    учтено штрафом в `arc.cost`; жёстко отсеиваются только дуги, не попавшие в граф.
/// 2. Сортируем допустимые дуги по **стоимости** (возрастание).
///    Внутри одинаковой стоимости — по `distance` (ближе лучше).
/// 3. Проходим по отсортированным дугам. Для каждой дуги:
///    - Проверяем остатки предложения и спроса.
///    - Для дуг с флагом `is_mass_unloading` применяем двухусловную станционную проверку
///      [`MIN_BATCH_FROM_MASS_STATION`] (подробнее см. комментарии в теле функции):
///      (A) `existing + station_remaining < MIN_BATCH` — пара неосуществима;
///      (B) назначение `qty` оставит застрявший остаток `< MIN_BATCH` и других
///          узлов на станции недостаточно для его дальнейшего распределения.
///    - Назначаем `min(remaining_supply[s], remaining_demand[d])` вагонов.
///    - Обновляем остатки.
///    - Прекращаем, когда весь спрос на погрузку закрыт или предложение исчерпано.
///
/// # Почему это хорошая отправная точка для ALNS
///
/// - Гарантированно допустимо по типу вагона и по ограничению MIN_BATCH (без пост-обработки).
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
        .filter(|(_, arc)| arc.car_type_ok)
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

    // --- Пред-вычисление: узлы предложения по парам (mass_station, demand_station) ---
    //
    // Для каждой пары (supply_station_code, demand_station_code) из mass_unloading дуг
    // собираем множество s_idx. Это позволяет в O(|узлов на станции|) вычислять
    // суммарный остаток по станции, а не по одному узлу.
    // Дедупликация по s_idx важна: одна пара станций может давать несколько дуг,
    // если demand-станция имеет несколько узлов (разные периоды, разные типы отправок и др.).
    let mut mass_pair_supply_idx: HashMap<(String, String), HashSet<usize>> = HashMap::new();
    for arc in arcs.iter().filter(|a| a.is_mass_unloading) {
        mass_pair_supply_idx
            .entry((arc.supply_station_code.clone(), arc.demand_station_code.clone()))
            .or_default()
            .insert(arc.s_idx);
    }

    // --- Жадное назначение ---
    let mut assignments: Vec<Assignment> = Vec::new();
    let mut total_cost:    f64 = 0.0;
    let mut assigned_cars: i32 = 0;

    // Суммарный назначенный поток по парам (supply_station_code, demand_station_code)
    // для дуг is_mass_unloading. Используется для проверки MIN_BATCH.
    let mut mass_pair_totals: HashMap<(String, String), i32> = HashMap::new();

    for arc_i in feasible_arc_indices {
        let arc = &arcs[arc_i];

        let avail_supply = remaining_supply[arc.s_idx];
        let avail_demand = remaining_demand[arc.d_idx];

        // Оба узла должны иметь остаток.
        if avail_supply <= 0 || avail_demand <= 0 {
            continue;
        }

        let qty = avail_supply.min(avail_demand);

        // Станционная inline-проверка ограничения MIN_BATCH.
        //
        // Правило: поток по паре (mass_station A → load_station B) должен быть
        // 0 или >= MIN_BATCH_FROM_MASS_STATION.
        //
        // Два условия пропуска дуги:
        //
        // (A) existing + station_remaining < MIN_BATCH
        //     Суммарный остаток по ВСЕМ узлам станции A с дугами до B плюс уже
        //     назначенный поток — меньше порога. Пара никогда не наберёт допустимую
        //     партию; дугу пропускаем.
        //     Это также позволяет объединять мелкие узлы одной станции: если у
        //     двух узлов по 2 вагона, station_remaining = 4 ≥ 3 → оба разрешены.
        //
        // (B) avail_supply > avail_demand
        //     AND residual (= avail_supply − qty) < MIN_BATCH
        //     AND (station_remaining − avail_supply) < MIN_BATCH
        //     После назначения qty вагонов спрос у узла D будет исчерпан (avail_demand
        //     вагонов ушли, больше места нет), у нас остаётся residual < MIN_BATCH,
        //     а других узлов на станции A тоже недостаточно чтобы открыть для
        //     остатка новую валидную пару. Лучше пропустить эту дугу и найти
        //     destination с avail_demand ≥ avail_supply, куда вагоны уйдут целиком.
        //
        // Если ни одно из условий не выполнено — назначение разрешается.
        if arc.is_mass_unloading {
            let key = (arc.supply_station_code.clone(), arc.demand_station_code.clone());
            let existing = mass_pair_totals.get(&key).copied().unwrap_or(0);
            let station_remaining: i32 = mass_pair_supply_idx
                .get(&key)
                .map(|nodes| nodes.iter().map(|&si| remaining_supply[si]).sum())
                .unwrap_or(0);

            // (A) пара неосуществима
            if existing + station_remaining < MIN_BATCH_FROM_MASS_STATION {
                continue;
            }

            // (B) назначение оставит застрявший остаток < MIN_BATCH
            let residual = avail_supply - qty; // > 0 только когда avail_supply > avail_demand
            let other_station_remaining = station_remaining - avail_supply; // ≥ 0
            if residual > 0
                && residual < MIN_BATCH_FROM_MASS_STATION
                && other_station_remaining < MIN_BATCH_FROM_MASS_STATION
            {
                continue;
            }

            *mass_pair_totals.entry(key).or_insert(0) += qty;
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

        // Ранний выход: закрыт спрос на **погрузку** (промывка — опциональная ёмкость).
        if demand
            .iter()
            .zip(remaining_demand.iter())
            .all(|(d, &r)| d.purpose != DemandPurpose::Load || r <= 0)
        {
            break;
        }
    }

    // Post-processing удалён: inline-проверка выше гарантирует, что все назначения
    // на mass-unloading дуги уже соответствуют ограничению MIN_BATCH.

    // --- Итоговая статистика ---
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

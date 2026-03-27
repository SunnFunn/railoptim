use std::collections::HashMap;

use crate::node::{DemandNode, SupplyNode, TariffNode};

// ---------------------------------------------------------------------------
// Дуга транспортной задачи
// ---------------------------------------------------------------------------

/// Дуга оптимизационной задачи: возможное назначение одного узла предложения
/// на один узел спроса.
///
/// Каждая дуга соответствует паре (SupplyNode, DemandNode), для которой
/// найден тариф. Совокупность всех дуг образует граф транспортной задачи,
/// на котором LP-солвер минимизирует суммарную стоимость перевозки.
#[derive(Debug, Clone)]
pub struct TaskArc {
    /// Порядковый номер дуги в плоском списке (используется как индекс LP-переменной).
    pub arc_id: usize,

    /// Позиция узла предложения в срезе `supply` (0-based).
    pub s_idx: usize,
    /// Позиция узла спроса в срезе `demand` (0-based).
    pub d_idx: usize,

    /// Код станции образования порожнего (откуда подсылаем).
    pub supply_station_code: String,
    /// Код станции погрузки (куда подсылаем).
    pub demand_station_code: String,

    /// Стоимость перевозки, руб.
    pub cost: f64,
    /// Расстояние, км.
    pub distance: i32,
    /// Нормативный срок подсыла, сут.
    pub delivery_days: i32,

    /// Срок подсыла вписывается в плановый период погрузки.
    pub period_ok: bool,
    /// Тип вагона совместим с требованиями узла спроса.
    pub car_type_ok: bool,
}

// ---------------------------------------------------------------------------
// Построение дуг
// ---------------------------------------------------------------------------

/// Строит список дуг транспортной задачи на основе узлов предложения, спроса
/// и тарифной матрицы.
///
/// Дуга создаётся для каждой пары (supply, demand), для которой найден тариф
/// по ключу `(supply.station_to_code, demand.station_code)`.
/// Пары без тарифа пропускаются.
pub fn build_task_arcs(
    supply: &[SupplyNode],
    demand: &[DemandNode],
    tariffs: &[TariffNode],
) -> Vec<TaskArc> {
    // Индекс тарифов: (код_откуда, код_куда) → TariffNode
    let tariff_index: HashMap<(&str, &str), &TariffNode> = tariffs
        .iter()
        .map(|t| ((t.station_from_code.as_str(), t.station_to_code.as_str()), t))
        .collect();

    let mut arcs = Vec::new();

    for (s_idx, s) in supply.iter().enumerate() {
        for (d_idx, d) in demand.iter().enumerate() {
            let key = (s.station_to_code.as_str(), d.station_code.as_str());

            if let Some(tariff) = tariff_index.get(&key) {
                let period_ok   = tariff.period_of_delivery <= period_max_days(d.period);
                let car_type_ok = car_type_compatible(
                    s.car_type.as_deref(),
                    d.car_type.as_deref(),
                );

                arcs.push(TaskArc {
                    arc_id: arcs.len(),
                    s_idx,
                    d_idx,
                    supply_station_code: s.station_to_code.clone(),
                    demand_station_code: d.station_code.clone(),
                    cost:          tariff.cost,
                    distance:      tariff.distance,
                    delivery_days: tariff.period_of_delivery,
                    period_ok,
                    car_type_ok,
                });
            }
        }
    }

    arcs
}

// ---------------------------------------------------------------------------
// Вспомогательные функции
// ---------------------------------------------------------------------------

/// Максимальный срок подсыла в сутках для каждого планового периода.
///
/// - Период 1: сут. 1–5   → max 5 сут.
/// - Период 2: сут. 6–8   → max 8 сут.
/// - Период 3: сут. 9–10  → max 10 сут.
/// - Период 4: сут. 11–15 → max 15 сут.
fn period_max_days(period: u8) -> i32 {
    match period {
        1 =>  5,
        2 =>  8,
        3 => 10,
        4 => 15,
        _ =>  0,
    }
}

/// Совместимость типа вагона с требованиями узла спроса.
///
/// - Спрос "БКТ" → предложение тоже должно быть "БКТ".
/// - Спрос "Прочие" / None → принимается любой тип вагона.
fn car_type_compatible(supply_type: Option<&str>, demand_type: Option<&str>) -> bool {
    match demand_type {
        Some(dt) if dt == "БКТ" => supply_type == Some("БКТ"),
        _ => true,
    }
}

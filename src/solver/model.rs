use std::collections::HashMap;

use crate::node::{DemandNode, SupplyNode, TariffNode};

// ---------------------------------------------------------------------------
// Константы ограничений
// ---------------------------------------------------------------------------

/// Минимальный допустимый размер партии вагонов, назначаемых с узла
/// **станции массовой выгрузки** (`is_mass_unloading == true`) на один
/// узел спроса. Значение 0 тоже допустимо (нет назначения вовсе).
///
/// Значение `x` на дуге должно удовлетворять: `x == 0 || x >= MIN_BATCH_FROM_MASS_STATION`.
pub const MIN_BATCH_FROM_MASS_STATION: i32 = 3;

/// Штраф к тарифу (руб.) за каждые полные сутки выхода за допустимое окно срока подсыла
/// `[L - 3, U + 3]` для предложений с [`SupplyNode::supply_period`] **не равным** 10.
/// Для `supply_period == 10` по-прежнему действует жёсткий отсев дуг без штрафа.
pub const PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB: f64 = 10_000.0;

/// Надбавка к стоимости дуг предложения с `supply_period == 10` (дислокация 2–10 суток).
///
/// Делает вагоны дислокации менее привлекательными для решателя по сравнению
/// с вагонами периода 1 (готовы сегодня). Если оба вагона могут закрыть один
/// узел спроса и разница в тарифе ≤ `PERIOD10_COST_SURCHARGE_RUB`, решатель
/// предпочтёт вагон периода 1.
///
/// Значение выбрано как ~20–30% среднего тарифа и существенно ниже
/// [`super::lp::PENALTY_COST`] — period=10 остаётся конкурентным там, где
/// period=1 объективно недоступен (нет тарифа, нарушение срока).
pub const PERIOD10_COST_SURCHARGE_RUB: f64 = 40_000.0;

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

    /// Срок подсыла в пределах окна `[L−3, U+3]` по периоду спроса без штрафа.
    /// Если `false`, для `supply_period != 10` дуга всё же допустима, но к тарифу добавлен штраф
    /// (см. [`PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB`]); для `supply_period == 10`
    /// в граф попадают только дуги с `period_ok == true`.
    pub period_ok: bool,
    /// Тип вагона совместим с требованиями узла спроса.
    pub car_type_ok: bool,
    /// Узел предложения находится на станции массовой выгрузки.
    /// На таких дугах поток допустим только как `0` или `>= MIN_BATCH_FROM_MASS_STATION`.
    pub is_mass_unloading: bool,
}

// ---------------------------------------------------------------------------
// Построение дуг
// ---------------------------------------------------------------------------

/// Строит список **допустимых** дуг транспортной задачи.
///
/// В LP попадают только пары, для которых одновременно выполнены:
/// - найден тариф по ключу `(supply.station_to_code, demand.station_code)`;
/// - тип вагона совместим с требованиями спроса (`car_type_ok`) — **жёстко**;
/// - срок подсыла:
///   - при [`SupplyNode::supply_period`] == 10 — как [`delivery_period_ok`]: дуга отбрасывается,
///     если срок не в окне (со сдвигом −5 сут);
///   - иначе (в т.ч. период 1 из АПИ) — дуга **не** отбрасывается: при выходе за `[L−3, U+3]`
///     к [`TariffNode::cost`] добавляется [`PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB`]
///     за каждую полную сутку нарушения; [`TaskArc::period_ok`] отражает отсутствие нарушения.
///
/// Недопустимые пары отсеиваются до решателя — размерность LP уменьшается.
/// Неудовлетворённый спрос обрабатывается slack-переменными в [`super::lp::solve`].
///
/// Возвращает `(arcs, stats)`, где `stats` — счётчики для диагностики.
pub fn build_task_arcs(
    supply: &[SupplyNode],
    demand: &[DemandNode],
    tariffs: &[TariffNode],
) -> (Vec<TaskArc>, ArcStats) {
    // Индекс тарифов: (код_откуда, код_куда) → TariffNode
    let tariff_index: HashMap<(&str, &str), &TariffNode> = tariffs
        .iter()
        .map(|t| ((t.station_from_code.as_str(), t.station_to_code.as_str()), t))
        .collect();

    let mut arcs       = Vec::new();
    let mut no_tariff  = 0usize;
    let mut bad_period = 0usize;
    let mut bad_type   = 0usize;
    let mut arcs_period_penalized = 0usize;

    for (s_idx, s) in supply.iter().enumerate() {
        for (d_idx, d) in demand.iter().enumerate() {
            let key = (s.station_to_code.as_str(), d.station_code.as_str());

            let Some(tariff) = tariff_index.get(&key) else {
                no_tariff += 1;
                continue;
            };

            let car_type_ok = car_type_compatible(s.car_type.as_deref(), d.car_type.as_deref());
            if !car_type_ok {
                bad_type += 1;
                continue;
            }

            let (period_ok, cost) = if s.supply_period == 10 {
                let ok = delivery_period_ok(
                    tariff.period_of_delivery,
                    d.period,
                    s.supply_period,
                );
                if !ok {
                    bad_period += 1;
                    continue;
                }
                (true, tariff.cost)
            } else {
                let Some(violation_days) =
                    delivery_window_violation_days(tariff.period_of_delivery, d.period)
                else {
                    bad_period += 1;
                    continue;
                };
                let period_ok = violation_days == 0;
                if violation_days > 0 {
                    arcs_period_penalized += 1;
                }
                let penalty = violation_days as f64 * PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB;
                (period_ok, tariff.cost + penalty)
            };

            // Вариант B: надбавка к стоимости дуг period=10 для приоритизации period=1.
            let cost = if s.supply_period == 10 {
                cost + PERIOD10_COST_SURCHARGE_RUB
            } else {
                cost
            };

            arcs.push(TaskArc {
                arc_id: arcs.len(),
                s_idx,
                d_idx,
                supply_station_code: s.station_to_code.clone(),
                demand_station_code: d.station_code.clone(),
                cost,
                distance:          tariff.distance,
                delivery_days:     tariff.period_of_delivery,
                period_ok,
                car_type_ok:       true,
                is_mass_unloading: s.is_mass_unloading,
            });
        }
    }

    let stats = ArcStats {
        total_pairs: supply.len() * demand.len(),
        no_tariff,
        bad_period,
        bad_type,
        feasible: arcs.len(),
        arcs_period_penalized,
    };

    (arcs, stats)
}

/// Диагностические счётчики из [`build_task_arcs`].
#[derive(Debug)]
pub struct ArcStats {
    /// Всего пар (supply × demand).
    pub total_pairs: usize,
    /// Пар без тарифа.
    pub no_tariff:  usize,
    /// Пар отсеяно по сроку подсыла (только жёсткий режим: нет границ периода или `supply_period == 10`).
    pub bad_period: usize,
    /// Пар с несовместимым типом вагона.
    pub bad_type:   usize,
    /// Допустимых дуг (вошли в LP).
    pub feasible:   usize,
    /// Дуг с ненулевым штрафом за срок подсыла (`supply_period != 10`, вне `[L−3, U+3]`).
    pub arcs_period_penalized: usize,
}

// ---------------------------------------------------------------------------
// Вспомогательные функции
// ---------------------------------------------------------------------------

/// Сутки погрузки по плановому периоду спроса: нижняя и верхняя граница включительно.
///
/// Значения соответствуют 0-based смещениям в [`crate::data::demand`]:
/// `DEMAND_PERIODS = [(0,4), (5,7), (8,9), (10,14)]`.
///
/// - Период 1: сут. 0–4  (сегодня + 0..4)
/// - Период 2: сут. 5–7
/// - Период 3: сут. 8–9
/// - Период 4: сут. 10–14
fn demand_period_day_bounds(period: u8) -> Option<(i32, i32)> {
    match period {
        1 => Some((0, 4)),
        2 => Some((5, 7)),
        3 => Some((8, 9)),
        4 => Some((10, 14)),
        _ => None,
    }
}

/// Допустим ли нормативный срок подсыла (`delivery_days`, сут.) для пары спрос/предложение.
///
/// Правило: по границам окна погрузки `[L, U]` допускается прибытие, если срок подсыла
/// попадает в `[L - 3, U + 3]` (три суток раньше нижней границы и три суток позже верхней,
/// граничные сутки периода входят в окно погрузки).
///
/// Для предложения с [`SupplyNode::supply_period`] == 10 (дислокация 2–10 суток) порожние
/// образуются на **5 суток позже**, чем у периода 1; то же окно для срока подсыла сдвигается
/// на −5 суток: проверяется `[L - 3 - 5, U + 3 - 5]`.
pub(crate) fn delivery_period_ok(
    delivery_days: i32,
    demand_period: u8,
    supply_period: u8,
) -> bool {
    let Some((l, u)) = demand_period_day_bounds(demand_period) else {
        return false;
    };
    let mut min_days = l - 3;
    let mut max_days = u + 3;
    if supply_period == 10 {
        min_days -= 5;
        max_days -= 5;
    }
    delivery_days >= min_days && delivery_days <= max_days
}

/// Число полных суток, на которое `delivery_days` выходит за окно `[L - 3, U + 3]` по периоду спроса.
/// `None`, если период спроса не сопоставлен с границами L, U.
fn delivery_window_violation_days(delivery_days: i32, demand_period: u8) -> Option<i32> {
    let (l, u) = demand_period_day_bounds(demand_period)?;
    let min_days = l - 3;
    let max_days = u + 3;
    if delivery_days < min_days {
        Some(min_days - delivery_days)
    } else if delivery_days > max_days {
        Some(delivery_days - max_days)
    } else {
        Some(0)
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

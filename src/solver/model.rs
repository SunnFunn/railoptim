use std::collections::{HashMap, HashSet};

use crate::data::references::normalize_etsng_code;
use crate::data::wash::{effective_etsng_for_wash_tariff, supply_needs_wash};
use crate::node::{DemandNode, DemandPurpose, SupplyNode, TariffNode};

// ---------------------------------------------------------------------------
// Константы ограничений
// ---------------------------------------------------------------------------

/// Минимальный допустимый размер партии вагонов, назначаемых с узлов
/// **станции массовой выгрузки** (`is_mass_unloading == true`) на узлы
/// одной станции погрузки. Значение 0 тоже допустимо (нет назначений между станциями вовсе).
///
/// Значение `x` на суммах дуг станция-станция должно удовлетворять: `x == 0 || x >= MIN_BATCH_FROM_MASS_STATION`.
pub const MIN_BATCH_FROM_MASS_STATION: i32 = 3;

/// Штраф к тарифу (руб.) за каждые полные сутки выхода за допустимое окно срока подсыла
/// `[L - 3, U + 3]` для предложений с [`SupplyNode::supply_period`] **не равным** 10.
pub const PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB: f64 = 15_000.0;

/// Штраф к тарифу (руб.) за каждые полные сутки нарушения окна для предложений
/// с [`SupplyNode::supply_period`] == 10 (дислокация 2–10 суток).
///
/// Вдвое выше [`PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB`], что отражает бо́льшую
/// неопределённость в сроках порожних из дислокации. Окно при этом сдвигается на −5 сут.:
/// проверяется `[L − 3 − 5, U + 3 − 5]`.
pub const PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_PERIOD10_RUB: f64 = 100_000.0;

/// Надбавка к стоимости дуг предложения с `supply_period == 10` (дислокация 2–10 суток).
///
/// Делает вагоны дислокации менее привлекательными для решателя по сравнению
/// с вагонами периода 1 (готовы сегодня). Если оба вагона могут закрыть один
/// узел спроса и разница в тарифе ≤ `PERIOD10_COST_SURCHARGE_RUB`, решатель
/// предпочтёт вагон периода 1.
///
/// [`super::lp::PENALTY_COST`] — period=10 остаётся конкурентным там, где
/// period=1 объективно недоступен (нет тарифа, нарушение срока).
pub const PERIOD10_COST_SURCHARGE_RUB: f64 = 500_000.0;

/// Средняя стоимость промывки вагона (руб.), добавляется к тарифу «до станции промывки»
/// для честного сравнения с назначением под погрузку аналогичного груза.
pub const WASH_PROCEDURE_AVG_COST_RUB: f64 = 10_000.0;

/// Средняя стоимость порожнего пробега после промывки до погрузки (руб.), добавляется к тарифу до промывки.
pub const EMPTY_RUN_AFTER_WASH_TO_LOAD_AVG_COST_RUB: f64 = 40_000.0;

/// Полная надбавка к тарифу до станции промывки для оптимизации.
pub const WASH_PATH_SURCHARGE_RUB: f64 =
    WASH_PROCEDURE_AVG_COST_RUB + EMPTY_RUN_AFTER_WASH_TO_LOAD_AVG_COST_RUB;

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
    /// со слабыми ограничениями поле не нужно
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
/// - период спроса имеет табличные границы — иначе дуга отбрасывается жёстко.
///
/// Нарушение допустимого окна срока подсыла — **мягкое** для всех периодов предложения:
/// - период 1: окно `[L−3, U+3]`, штраф [`PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB`]/сут.
/// - период 10: окно `[L−3−5, U+3−5]` (сдвиг −5 сут.), штраф
///   [`PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_PERIOD10_RUB`]/сут. (вдвое выше).
///
/// [`TaskArc::period_ok`] == `true` означает, что нарушения окна нет.
/// Неудовлетворённый спрос обрабатывается slack-переменными в [`super::lp::solve`].
///
/// Возвращает `(arcs, stats)`, где `stats` — счётчики для диагностики.
///
/// `tariffs` — тарифы до станций **погрузки** (как из АПИ).
/// `wash_tariffs` — тарифы до станций **промывки** с уже учтённой надбавкой
/// [`WASH_PATH_SURCHARGE_RUB`] (промывка + порожний пробег до погрузки), ключ `(откуда, куда)`.
pub fn build_task_arcs(
    supply: &[SupplyNode],
    demand: &[DemandNode],
    tariffs: &[TariffNode],
    wash_codes: &HashSet<String>,
    no_cleaning_roads: &HashSet<String>,
    wash_tariffs: &HashMap<(String, String), TariffNode>,
) -> (Vec<TaskArc>, ArcStats) {
    // Индекс тарифов погрузки: (код_откуда, код_куда) → TariffNode
    let tariff_index: HashMap<(&str, &str), &TariffNode> = tariffs
        .iter()
        .map(|t| ((t.station_from_code.as_str(), t.station_to_code.as_str()), t))
        .collect();

    let mut arcs       = Vec::new();
    let mut no_tariff  = 0usize;
    let mut bad_period = 0usize;
    let mut bad_type   = 0usize;
    let mut dirty_etsng_mismatch = 0usize;
    let mut arcs_period_penalized = 0usize;

    for (s_idx, s) in supply.iter().enumerate() {
        for (d_idx, d) in demand.iter().enumerate() {
            let tariff: &TariffNode = match d.purpose {
                DemandPurpose::Wash => {
                    // Вагоны с дорогой образования из NoCleaningRoads — не грязные
                    // (промывка уже оплачена клиентом на иностранной территории).
                    if !supply_needs_wash(s, wash_codes, no_cleaning_roads) {
                        no_tariff += 1;
                        continue;
                    }
                    let key = (s.station_to_code.clone(), d.station_code.clone());
                    let Some(t) = wash_tariffs.get(&key) else {
                        no_tariff += 1;
                        continue;
                    };
                    t
                }
                DemandPurpose::Load => {
                    // Ограничение «грязного» вагона:
                    // если вагон из-под груза, требующего промывки (и не освобождён
                    // по NoCleaningRoads), он может быть назначен под погрузку
                    // ТОЛЬКО под тот же ЕТСНГ.
                    // Альтернативный маршрут — через узел промывки (DemandPurpose::Wash).
                    if supply_needs_wash(s, wash_codes, no_cleaning_roads) {
                        let supply_etsng = effective_etsng_for_wash_tariff(s);
                        let demand_etsng = d.etsng.as_deref().map(normalize_etsng_code);
                        match (supply_etsng, demand_etsng) {
                            (Some(se), Some(de)) if se == de => {} // ETSNG совпадает → дуга разрешена
                            _ => {
                                dirty_etsng_mismatch += 1;
                                continue;
                            }
                        }
                    }

                    let key = (s.station_to_code.as_str(), d.station_code.as_str());
                    let Some(t) = tariff_index.get(&key) else {
                        no_tariff += 1;
                        continue;
                    };
                    *t
                }
            };

            let car_type_ok = car_type_compatible(s.car_type.as_deref(), d.car_type.as_deref());
            if !car_type_ok {
                bad_type += 1;
                continue;
            }

            let (period_ok, cost) = {
                let penalty_rate = if s.supply_period == 10 {
                    PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_PERIOD10_RUB
                } else {
                    PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB
                };
                let Some(violation_days) = delivery_window_violation_days(
                    tariff.period_of_delivery,
                    d.period,
                    s.supply_period,
                ) else {
                    bad_period += 1;
                    continue;
                };
                let period_ok = violation_days == 0;
                if violation_days > 0 {
                    arcs_period_penalized += 1;
                }
                let penalty = violation_days as f64 * penalty_rate;
                (period_ok, tariff.cost + penalty)
            };

            // надбавка к стоимости дуг period=10 для приоритизации period=1.
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
                // Ограничение MIN_BATCH только для погрузки, не для промывки.
                is_mass_unloading: s.is_mass_unloading && d.purpose == DemandPurpose::Load,
            });
        }
    }

    let stats = ArcStats {
        total_pairs: supply.len() * demand.len(),
        no_tariff,
        bad_period,
        bad_type,
        dirty_etsng_mismatch,
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
    /// Пар «грязный» вагон → погрузка с несовпадающим ЕТСНГ (запрещено без промывки).
    pub dirty_etsng_mismatch: usize,
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
/// попадает в `[L - 3, U + 3]` (трое суток раньше нижней границы и трое суток позже верхней,
/// граничные сутки периода входят в окно погрузки).
///
/// Для предложения с [`SupplyNode::supply_period`] == 10 (дислокация 2–10 суток) порожние
/// образуются на **5 суток позже**, чем у периода 1; то же окно для срока подсыла сдвигается
/// на −5 суток: проверяется `[L - 3 - 5, U + 3 - 5]`.
// pub(crate) fn delivery_period_ok(
//     delivery_days: i32,
//     demand_period: u8,
//     supply_period: u8,
// ) -> bool {
//     let Some((l, u)) = demand_period_day_bounds(demand_period) else {
//         return false;
//     };
//     let mut min_days = l - 3;
//     let mut max_days = u + 3;
//     if supply_period == 10 {
//         min_days -= 5;
//         max_days -= 5;
//     }
//     delivery_days >= min_days && delivery_days <= max_days
// }

/// Число полных суток, на которое `delivery_days` выходит за допустимое окно по периоду спроса.
///
/// Окно для `supply_period != 10`: `[L − 3, U + 3]`.
/// Окно для `supply_period == 10`: `[L − 3 − 5, U + 3 − 5]` (сдвиг −5 сут., т.к.
/// порожние из дислокации освобождаются в среднем на 5 суток позже).
///
/// Возвращает `None`, если период спроса не имеет табличных границ L, U.
fn delivery_window_violation_days(
    delivery_days: i32,
    demand_period: u8,
    supply_period:  u8,
) -> Option<i32> {
    let (l, u) = demand_period_day_bounds(demand_period)?;
    let shift    = if supply_period == 10 { 5 } else { 0 };
    let min_days = l - 3 - shift;
    let max_days = u + 3 - shift;
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

// ---------------------------------------------------------------------------
// Проверка ограничения MIN_BATCH на уровне пары станций
// ---------------------------------------------------------------------------

/// Возвращает пары станций `(supply_station_code, demand_station_code)`, для которых
/// суммарный поток из mass-unloading источника нарушает ограничение:
/// `0 < total < MIN_BATCH_FROM_MASS_STATION`.
///
/// Принимает итератор `(arc_id, quantity)` — не зависит от конкретного типа назначения,
/// что позволяет использовать функцию как из `greedy.rs`, так и из `alns.rs`.
///
/// `arc_id` должен соответствовать индексу в срезе `arcs` (`arc.arc_id == index`).
pub fn collect_mass_pair_violations(
    flow: impl Iterator<Item = (usize, i32)>,
    arcs: &[TaskArc],
) -> Vec<(String, String)> {
    let mut totals: HashMap<(&str, &str), i32> = HashMap::new();
    for (arc_id, quantity) in flow {
        let arc = &arcs[arc_id];
        if arc.is_mass_unloading {
            *totals
                .entry((arc.supply_station_code.as_str(), arc.demand_station_code.as_str()))
                .or_insert(0) += quantity;
        }
    }
    totals
        .into_iter()
        .filter(|(_, total)| *total > 0 && *total < MIN_BATCH_FROM_MASS_STATION)
        .map(|((s, d), _)| (s.to_string(), d.to_string()))
        .collect()
}

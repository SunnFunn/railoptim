//! Правило 4 бизнес-правил: загруженность станции погрузки.
//!
//! Станция описывается как очередь с известной скоростью обслуживания:
//!   * `C` — мощность погрузки, ваг./сут. (`station_load_capacity` из
//!     `data/load_stations.json`, колонка U исходного Excel);
//!   * `Q` — вагонов уже на станции (`CarsOnStation` из АПИ спроса, поле
//!     [`DemandNode::cars_on_station`]; по станции берётся максимум по её узлам).
//!
//! **Жёсткая часть** (`StationBacklogHardDays` = `K_hard`): `Q > K_hard · C` → станция
//! закрыта для подсыла во все периоды ([`crate::solver::model::PairOutcome::StationOverloaded`]).
//! Такая очередь — признак остановившейся погрузки (отказ принимать груз, вагоны
//! стоят неделями), и предполагать, что она рассосётся со скоростью `C`, нельзя.
//! Прогон суточный, поэтому закрытие дальних периодов почти ничего не стоит:
//! если станция разгрузится, следующий прогон её снова откроет.
//!
//! **Мягкая часть** (`StationBacklogSoftDays` = `K_soft` < `K_hard`): очередь
//! рассасывается со скоростью `C`, и сутки
//! `t* = max(0, ceil((Q − K_soft · C) / C))` — момент, когда на станции останется
//! не больше `K_soft` суток работы. Вагон, прибывающий раньше `t*`, ждёт до `t*`:
//! ожидаемые сутки погрузки `max(прибытие, t*)` проверяются окном периода спроса
//! (см. `delivery_window_violation_days`), а за каждые сутки ожидания к тарифу
//! добавляется `StationBacklogWaitPenaltyRubPerDay`.
//!
//! Правило действует только на спрос **погрузки**. Станции с `C = 0` (мощность
//! неизвестна: пустая/текстовая ячейка Excel) или без записи в справочнике не
//! проверяются. Правило целиком отключается, если `StationBacklogHardDays` не задан.
//!
//! TODO: в `Q` пока входят только вагоны, уже стоящие на станции. Вагоны **в пути**
//! на станцию АПИ отдельным полем не отдаёт (`ProvidedCarsToLoad` — совокупный
//! счётчик: погружено + в пути + на станции + назначено, для очереди не годится);
//! когда поле появится, прибавлять его к `Q` здесь, в [`StationBacklogIndex::build`].

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::business_rules::BusinessRules;
use super::esr::normalize_esr6;
use crate::node::{DemandNode, DemandPurpose};

/// Путь к справочнику станций погрузки (тот же, что у свободных ёмкостей путей).
pub const DEFAULT_LOAD_STATIONS_PATH: &str = super::free_loadroads::DEFAULT_LOAD_STATIONS_PATH;

/// Строка `data/load_stations.json` — только нужные поля.
#[derive(Debug, Clone, Deserialize)]
struct LoadStationCapacityRow {
    #[serde(default)]
    load_station_code: Option<String>,
    #[serde(default)]
    station_load_capacity: i64,
}

/// Загруженность одной станции погрузки (результат расчёта по правилу 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationBacklog {
    /// `C` — мощность погрузки, ваг./сут. (всегда > 0: станции с 0 в индекс не попадают).
    pub load_capacity: i32,
    /// `Q` — вагонов уже на станции.
    pub cars_on_station: i32,
    /// Жёсткая часть: `Q > K_hard · C` — подсыл запрещён во все периоды.
    pub closed: bool,
    /// Мягкая часть: сутки (от сегодня), когда очередь опустится до `K_soft` суток работы.
    /// Вагон, прибывающий раньше, ждёт до этих суток.
    pub backlog_clear_day: i32,
}

impl StationBacklog {
    /// Сутки ожидания погрузки для вагона, прибывающего на станцию в `arrival_day`
    /// (сутки от сегодня). Для закрытой станции не имеет смысла (дуга не создаётся).
    pub fn wait_days(&self, arrival_day: i32) -> i32 {
        (self.backlog_clear_day - arrival_day).max(0)
    }
}

/// Статистика построения индекса — для логов суточного прогона.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StationBacklogStats {
    /// Станций с известной мощностью (`C > 0`) в справочнике.
    pub capacity_stations: usize,
    /// Станций с Load-спросом.
    pub demand_stations: usize,
    /// Из них проверено правилом (мощность известна).
    pub checked_stations: usize,
    /// Из них без мощности (нет в справочнике или `C = 0`) — правило не применяется.
    pub unknown_capacity_stations: usize,
    /// Закрытых станций (жёсткая часть).
    pub closed_stations: usize,
    /// Узлов и вагонов Load-спроса на закрытых станциях.
    pub closed_demand_nodes: usize,
    pub closed_demand_cars: i32,
    /// Открытых станций с очередью (`t* > 0`) — мягкая часть сдвигает погрузку.
    pub waiting_stations: usize,
    /// Станций, у узлов которых `CarsOnStation` различается (проверка семантики АПИ).
    pub inconsistent_q_stations: usize,
    /// Закрытые станции для лога: (название, дорога, Q, C), по убыванию Q/C.
    pub closed_list: Vec<(String, String, i32, i32)>,
}

/// Индекс загруженности станций погрузки по коду ЕСР-6.
///
/// Пустой индекс ([`Self::disabled`]) — правило не действует: `get` всегда `None`.
#[derive(Debug, Clone, Default)]
pub struct StationBacklogIndex {
    by_code: HashMap<String, StationBacklog>,
    wait_penalty_rub_per_day: f64,
    pub stats: StationBacklogStats,
}

impl StationBacklogIndex {
    /// Правило отключено: ни одна станция не ограничивается.
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.by_code.is_empty()
    }

    /// Число станций в индексе (с известной мощностью и Load-спросом).
    pub fn len(&self) -> usize {
        self.by_code.len()
    }

    /// Штраф за сутки ожидания погрузки (руб./ваг.).
    pub fn wait_penalty_rub_per_day(&self) -> f64 {
        self.wait_penalty_rub_per_day
    }

    /// Загруженность станции по коду ЕСР (в любом написании). `None` — правило к
    /// станции не применяется (нет мощности, нет спроса или правило отключено).
    pub fn get(&self, station_code: &str) -> Option<&StationBacklog> {
        if self.by_code.is_empty() {
            return None;
        }
        self.by_code.get(&normalize_esr6(station_code))
    }

    /// Читает мощности из `load_stations.json` и строит индекс по узлам спроса.
    /// Если правило отключено в `rules`, файл не читается.
    pub fn load_and_build(
        path: impl AsRef<Path>,
        demand: &[DemandNode],
        rules: &BusinessRules,
    ) -> Result<Self> {
        if !rules.station_backlog_enabled() {
            return Ok(Self::disabled());
        }
        let capacities = load_capacities(path)?;
        Ok(Self::build(&capacities, demand, rules))
    }

    /// Чистое построение индекса: `capacities` — код ЕСР-6 → `C` (только `C > 0`).
    pub fn build(
        capacities: &HashMap<String, i32>,
        demand: &[DemandNode],
        rules: &BusinessRules,
    ) -> Self {
        let Some(hard_days) = rules.station_backlog_hard_days else {
            return Self::disabled();
        };
        let soft_days = rules.station_backlog_soft_days.max(0);

        // Q по станции: максимум CarsOnStation по её Load-узлам; попутно спрос по станции.
        struct Agg {
            name: String,
            railway: String,
            q_min: i32,
            q_max: i32,
            nodes: usize,
            cars: i32,
        }
        let mut by_station: HashMap<String, Agg> = HashMap::new();
        for d in demand.iter().filter(|d| d.purpose == DemandPurpose::Load) {
            let code = normalize_esr6(&d.station_code);
            if code.is_empty() {
                continue;
            }
            let q = d.cars_on_station.max(0);
            let e = by_station.entry(code).or_insert_with(|| Agg {
                name: d.station_name.clone(),
                railway: d.railway_name.clone(),
                q_min: q,
                q_max: q,
                nodes: 0,
                cars: 0,
            });
            e.q_min = e.q_min.min(q);
            e.q_max = e.q_max.max(q);
            e.nodes += 1;
            e.cars += d.car_count;
        }

        let mut stats = StationBacklogStats {
            capacity_stations: capacities.values().filter(|c| **c > 0).count(),
            demand_stations: by_station.len(),
            ..Default::default()
        };
        let mut by_code = HashMap::new();

        for (code, agg) in by_station {
            let Some(&capacity) = capacities.get(&code).filter(|c| **c > 0) else {
                stats.unknown_capacity_stations += 1;
                continue;
            };
            stats.checked_stations += 1;
            if agg.q_min != agg.q_max {
                stats.inconsistent_q_stations += 1;
            }
            let q = agg.q_max;
            let backlog = compute_backlog(capacity, q, hard_days, soft_days);
            if backlog.closed {
                stats.closed_stations += 1;
                stats.closed_demand_nodes += agg.nodes;
                stats.closed_demand_cars += agg.cars;
                stats.closed_list.push((agg.name, agg.railway, q, capacity));
            } else if backlog.backlog_clear_day > 0 {
                stats.waiting_stations += 1;
            }
            by_code.insert(code, backlog);
        }

        // Самые забитые — первыми (по отношению Q/C), затем по имени для детерминизма.
        stats.closed_list.sort_by(|a, b| {
            let ra = a.2 as f64 / a.3 as f64;
            let rb = b.2 as f64 / b.3 as f64;
            rb.partial_cmp(&ra)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        Self {
            by_code,
            wait_penalty_rub_per_day: rules.station_backlog_wait_penalty_rub_per_day.max(0.0),
            stats,
        }
    }
}

/// Расчёт загруженности одной станции (`capacity > 0`).
fn compute_backlog(capacity: i32, cars_on_station: i32, hard_days: i32, soft_days: i32) -> StationBacklog {
    let c = capacity.max(1) as i64;
    let q = cars_on_station.max(0) as i64;
    let closed = q > hard_days.max(0) as i64 * c;
    let excess = q - soft_days.max(0) as i64 * c;
    let backlog_clear_day = if excess <= 0 { 0 } else { (excess + c - 1) / c };
    StationBacklog {
        load_capacity: capacity,
        cars_on_station: cars_on_station.max(0),
        closed,
        backlog_clear_day: backlog_clear_day.min(i32::MAX as i64) as i32,
    }
}

/// Мощности погрузки из `load_stations.json`: код ЕСР-6 → `C` (ваг./сут.).
///
/// Записи без кода или с `C <= 0` пропускаются; несколько записей с одним кодом
/// (разные названия одной станции) суммируются — как элеваторы одной станции в
/// `build_load_stations`.
pub fn load_capacities(path: impl AsRef<Path>) -> Result<HashMap<String, i32>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("чтение {}", path.display()))?;
    let rows: Vec<LoadStationCapacityRow> =
        serde_json::from_str(&text).with_context(|| format!("разбор {}", path.display()))?;
    Ok(capacities_from_rows(rows.iter().map(|r| (r.load_station_code.as_deref(), r.station_load_capacity))))
}

fn capacities_from_rows<'a>(
    rows: impl Iterator<Item = (Option<&'a str>, i64)>,
) -> HashMap<String, i32> {
    let mut map: HashMap<String, i64> = HashMap::new();
    for (code, cap) in rows {
        if cap <= 0 {
            continue;
        }
        let Some(code) = code else { continue };
        let code = normalize_esr6(code);
        if code.len() != 6 {
            continue;
        }
        *map.entry(code).or_insert(0) += cap;
    }
    map.into_iter()
        .map(|(k, v)| (k, v.min(i32::MAX as i64) as i32))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(hard: Option<i32>, soft: i32, penalty: f64) -> BusinessRules {
        BusinessRules {
            station_backlog_hard_days: hard,
            station_backlog_soft_days: soft,
            station_backlog_wait_penalty_rub_per_day: penalty,
            ..Default::default()
        }
    }

    fn demand(code: &str, cars: i32, q: i32) -> DemandNode {
        DemandNode {
            d_id: 0,
            purpose: DemandPurpose::Load,
            period: 1,
            station_name: format!("ст. {code}"),
            station_code: code.to_string(),
            railway_name: "ПРВ".to_string(),
            railway_code: None,
            railway_part: None,
            station_to_name: None,
            station_to_code: None,
            railway_to_name: None,
            railway_to_code: None,
            railway_to_part: None,
            sender: None,
            sender_okpo: None,
            sender_tgnl: None,
            client: None,
            customer_okpo: None,
            recipient: None,
            loader_to_okpo: None,
            gng_cargo: None,
            etsng: None,
            request_numbers: None,
            request_dates: None,
            gu12_number: None,
            shipping_type: None,
            car_type: None,
            car_count: cars,
            cars_on_station: q,
        }
    }

    fn caps(list: &[(&str, i32)]) -> HashMap<String, i32> {
        list.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn compute_backlog_hard_and_soft_thresholds() {
        // C = 10, K_hard = 5, K_soft = 1.
        // Q = 50 — ровно 5 суток работы: НЕ закрыта (строго больше), t* = ceil((50-10)/10) = 4.
        let b = compute_backlog(10, 50, 5, 1);
        assert!(!b.closed);
        assert_eq!(b.backlog_clear_day, 4);
        // Q = 51 → закрыта.
        assert!(compute_backlog(10, 51, 5, 1).closed);
        // Q = 10 — одни сутки работы = допустимая очередь: t* = 0.
        assert_eq!(compute_backlog(10, 10, 5, 1).backlog_clear_day, 0);
        // Q = 11 → t* = ceil(1/10) = 1.
        assert_eq!(compute_backlog(10, 11, 5, 1).backlog_clear_day, 1);
        // Q = 0 → всё свободно; отрицательное Q трактуется как 0.
        assert_eq!(compute_backlog(10, 0, 5, 1).backlog_clear_day, 0);
        assert_eq!(compute_backlog(10, -3, 5, 1).cars_on_station, 0);
        // K_soft = 0: очередь должна быть пустой, t* = ceil(Q/C).
        assert_eq!(compute_backlog(10, 25, 5, 0).backlog_clear_day, 3);
    }

    #[test]
    fn wait_days_from_arrival() {
        let b = compute_backlog(10, 45, 5, 1); // t* = 4
        assert_eq!(b.wait_days(0), 4);
        assert_eq!(b.wait_days(3), 1);
        assert_eq!(b.wait_days(4), 0);
        assert_eq!(b.wait_days(9), 0);
    }

    #[test]
    fn build_index_closed_waiting_unknown() {
        let capacities = caps(&[("100001", 10), ("100002", 10), ("100004", 0)]);
        let demand = vec![
            demand("100001", 20, 60), // закрыта: 60 > 50
            demand("100001", 5, 60),
            demand("100002", 7, 30),  // открыта, t* = 2
            demand("100003", 7, 500), // нет в справочнике → не проверяется
            demand("100004", 7, 500), // C = 0 → не проверяется
        ];
        let idx = StationBacklogIndex::build(&capacities, &demand, &rules(Some(5), 1, 5000.0));
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.wait_penalty_rub_per_day(), 5000.0);

        let closed = idx.get("100001").unwrap();
        assert!(closed.closed);
        assert_eq!(closed.cars_on_station, 60);
        let waiting = idx.get("100002").unwrap();
        assert!(!waiting.closed);
        assert_eq!(waiting.backlog_clear_day, 2);
        assert!(idx.get("100003").is_none());
        assert!(idx.get("100004").is_none());
        // Код в другом написании нормализуется.
        assert!(idx.get(" 100001 ").is_some());

        let st = &idx.stats;
        assert_eq!(st.capacity_stations, 2);
        assert_eq!(st.demand_stations, 4);
        assert_eq!(st.checked_stations, 2);
        assert_eq!(st.unknown_capacity_stations, 2);
        assert_eq!(st.closed_stations, 1);
        assert_eq!(st.closed_demand_nodes, 2);
        assert_eq!(st.closed_demand_cars, 25);
        assert_eq!(st.waiting_stations, 1);
        assert_eq!(st.inconsistent_q_stations, 0);
        assert_eq!(st.closed_list.len(), 1);
        assert_eq!(st.closed_list[0].2, 60);
        assert_eq!(st.closed_list[0].3, 10);
    }

    #[test]
    fn q_is_max_over_station_nodes_and_inconsistency_counted() {
        let capacities = caps(&[("100001", 10)]);
        let demand = vec![demand("100001", 5, 12), demand("100001", 5, 55)];
        let idx = StationBacklogIndex::build(&capacities, &demand, &rules(Some(5), 1, 0.0));
        let b = idx.get("100001").unwrap();
        assert_eq!(b.cars_on_station, 55);
        assert!(b.closed);
        assert_eq!(idx.stats.inconsistent_q_stations, 1);
    }

    #[test]
    fn wash_demand_ignored() {
        let capacities = caps(&[("100001", 10)]);
        let mut wash = demand("100001", 5, 999);
        wash.purpose = DemandPurpose::Wash;
        let idx = StationBacklogIndex::build(&capacities, &[wash], &rules(Some(5), 1, 0.0));
        assert!(idx.is_empty());
        assert_eq!(idx.stats.demand_stations, 0);
    }

    #[test]
    fn disabled_when_hard_days_missing() {
        let capacities = caps(&[("100001", 10)]);
        let demand = vec![demand("100001", 5, 999)];
        let idx = StationBacklogIndex::build(&capacities, &demand, &rules(None, 1, 0.0));
        assert!(idx.is_empty());
        assert!(idx.get("100001").is_none());
        assert!(StationBacklogIndex::disabled().get("100001").is_none());
    }

    #[test]
    fn capacities_from_rows_skips_unknown_and_sums_duplicates() {
        let rows = vec![
            (Some("100001"), 10_i64),
            (Some("100001"), 5),   // та же станция под другим именем — суммируется
            (Some("100002"), 0),   // мощность неизвестна
            (None, 40),            // без кода ЕСР
            (Some("abc"), 40),     // не код ЕСР
            (Some("1234567"), 40), // не ЕСР-6 (7 знаков)
            (Some("12"), 40),      // короткий код дополняется нулями → 000012
        ];
        let m = capacities_from_rows(rows.into_iter());
        assert_eq!(m.len(), 2);
        assert_eq!(m["100001"], 15);
        assert_eq!(m["000012"], 40);
    }

    /// Боевой справочник читается, мощности положительные.
    #[test]
    fn repo_load_stations_json_has_capacities() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/load_stations.json");
        let m = load_capacities(&path).unwrap();
        assert!(m.len() > 100, "ожидается справочник с сотнями станций, получено {}", m.len());
        assert!(m.values().all(|c| *c > 0));
    }
}

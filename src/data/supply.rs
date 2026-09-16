use std::collections::{HashMap, HashSet};

use chrono::Utc;
use serde::Deserialize;

use crate::node::{CarKind, RepairStatus, SupplyNode};
use super::business_rules::BusinessRules;
use super::client::{ApiClient, ApiEndpoint, ApiError};

/// Минимальное суммарное количество вагонов на станции назначения,
/// при котором станция считается станцией массовой выгрузки.
const MASS_UNLOADING_THRESHOLD: i32 = 100;

// ---------------------------------------------------------------------------
// Внутренние структуры десериализации
// ---------------------------------------------------------------------------

/// Один элемент верхнего уровня ответа GetSupplyDataTransmission (одна дорога).
#[derive(Deserialize, Debug)]
struct SupplyApiItem {
    #[serde(rename = "opzCarNumberModelCollection", default)]
    numbered: Vec<NumberedCarItem>,

    #[serde(rename = "opzNoNumberModelCollection", default)]
    no_number: Vec<NoNumberItem>,
}

/// Номерной вагон из `opzCarNumberModelCollection`.
#[derive(Deserialize, Debug)]
struct NumberedCarItem {
    #[serde(rename = "CarNumber")] car_number: u64,

    // Станция и дорога отправления
    #[serde(rename = "StationFrom",      default)] station_from:       Option<String>,
    #[serde(rename = "StationFromCode",  default)] station_from_code:  Option<String>,
    #[serde(rename = "RailWayFromShort", default)] railway_from_short: Option<String>,
    #[serde(rename = "RailWayFromCode",  default)] railway_from_code:  Option<i32>,
    #[serde(rename = "RailWayPartFrom",  default)] railway_part_from:  Option<String>,

    // Станция и дорога назначения
    #[serde(rename = "StationTo",      default)] station_to:       Option<String>,
    #[serde(rename = "StationToCode",  default)] station_to_code:  Option<String>,
    #[serde(rename = "RailWayToShort", default)] railway_to_short: Option<String>,
    #[serde(rename = "RailWayToCode",  default)] railway_to_code:  Option<i32>,
    #[serde(rename = "RailWayPartTo",  default)] railway_part_to:  Option<String>,

    // OPZ-назначение: null = свободен (Free), не null = идёт по факту (Assigned)
    #[serde(rename = "OPZRailWayId")] opz_railway_id: Option<i64>,

    // Тип вагона
    #[serde(rename = "OPZComment1", default)] opz_c1: Option<String>,

    // Груз
    #[serde(rename = "GRPOName",        default)] grpo_name:       Option<String>,
    #[serde(rename = "FrETSNGCode",     default)] etsng:           Option<String>,
    #[serde(rename = "FrETSNGName",     default)] etsng_name:      Option<String>,
    #[serde(rename = "PrevFrETSNGCode", default)] prev_etsng:      Option<String>,
    #[serde(rename = "PrevFrETSNGName", default)] prev_etsng_name: Option<String>,

    // Ремонт
    #[serde(rename = "CarNextRepairDays",     default)] days_to_repair: Option<f64>,
    #[serde(rename = "CarNextRepairTypeName", default)] repair_type:    Option<String>,
    /// true — вагон подлежит ремонту по признаку АПИ.
    #[serde(rename = "IsCarRepair", default)] is_car_repair: bool,
    /// Цель назначения из DislocationPreview (дислокация); в ответе АПИ нет.
    #[serde(rename = "ShipmentGoalId", default)] shipment_goal_id: Option<i32>,
}

impl NumberedCarItem {
    fn car_type(&self) -> Option<String> {
        self.opz_c1.as_deref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    fn repair_status(&self, rules: &BusinessRules) -> RepairStatus {
        let railway = self.railway_to_short.as_deref().unwrap_or("");
        if rules.wagon_needs_repair(self.is_car_repair, self.days_to_repair, railway) {
            RepairStatus::NeedsRepair
        } else {
            RepairStatus::Ok
        }
    }
}

/// Безномерной вагон из `opzNoNumberModelCollection`.
#[derive(Deserialize, Debug)]
struct NoNumberItem {
    #[serde(rename = "StationToCode",  default)] station_to_code:  Option<String>,
    #[serde(rename = "StationTo",      default)] station_to:       Option<String>,
    #[serde(rename = "RailWayToShort", default)] railway_to_short: Option<String>,
    #[serde(rename = "RailWayToCode",  default)] railway_to_code:  Option<i32>,
    #[serde(rename = "RailWayPartTo",  default)] railway_part_to:  Option<String>,
    #[serde(rename = "FrETSNGCode",    default)] etsng:            Option<String>,
    #[serde(rename = "FrETSNGName",    default)] etsng_name:       Option<String>,
    #[serde(rename = "CarCount",       default)] car_count:        i32,
}

// ---------------------------------------------------------------------------
// Группировка
// ---------------------------------------------------------------------------

/// Ключ группировки для агрегации вагонов в узлы предложения.
#[derive(Hash, Eq, PartialEq, Clone)]
struct GroupKey {
    kind_ord:        u8,   // 0=Free, 1=Assigned, 2=NoNumber
    /// 1 — АПИ; 10 — дислокация 2–10 суток.
    supply_period:   u8,
    station_to:      String,
    station_to_code: String,
    railway_to:      String,
    railway_to_code: Option<i32>,
    railway_part_to: Option<String>,
    car_type:        Option<String>,
    etsng:           Option<String>,
    etsng_name:      Option<String>,
    needs_repair:    bool,
    status:          Option<String>,
}

/// Накопитель данных для одной группы.
struct GroupData {
    car_count:          i32,
    car_numbers:        Vec<u64>,
    stations_from:      Vec<String>,
    stations_from_code: Vec<String>,
    railways_from:      Vec<String>,
    railways_from_code: Vec<i32>,
    railways_part_from: Vec<String>,
    prev_etsngs:        Vec<String>,
    prev_etsng_names:   Vec<String>,
}

impl GroupData {
    fn new() -> Self {
        Self {
            car_count: 0,
            car_numbers: vec![],
            stations_from: vec![], stations_from_code: vec![],
            railways_from: vec![], railways_from_code: vec![],
            railways_part_from: vec![],
            prev_etsngs: vec![], prev_etsng_names: vec![],
        }
    }
}

fn kind_from_ord(ord: u8) -> CarKind {
    match ord {
        1 => CarKind::Assigned,
        2 => CarKind::NoNumber,
        _ => CarKind::Free,
    }
}

fn kind_to_ord(kind: &CarKind) -> u8 {
    match kind { CarKind::Free => 0, CarKind::Assigned => 1, CarKind::NoNumber => 2 }
}

/// Группирует плоский список вагонов в агрегированные узлы предложения.
///
/// `supply_period`: `1` для данных АПИ, `10` для дислокации (2–10 сутки).
/// `rules` — правило 7 (горизонт вывода в ремонт, в т.ч. 45 сут. на инотерритории).
fn group_supply(
    numbered: impl Iterator<Item = NumberedCarItem>,
    no_number: impl Iterator<Item = NoNumberItem>,
    supply_period: u8,
    rules: &BusinessRules,
) -> Vec<SupplyNode> {
    let mut groups: HashMap<GroupKey, GroupData> = HashMap::new();
    // Сохраняем порядок первого появления ключа.
    let mut key_order: Vec<GroupKey> = Vec::new();

    // --- Именные вагоны ---
    for c in numbered {
        let car_type = c.car_type();
        let repair   = c.repair_status(rules);
        let kind     = if c.opz_railway_id.is_some() { CarKind::Assigned } else { CarKind::Free };

        let key = GroupKey {
            kind_ord:        kind_to_ord(&kind),
            supply_period,
            station_to:      c.station_to.clone().unwrap_or_default(),
            station_to_code: c.station_to_code.clone().unwrap_or_default(),
            railway_to:      c.railway_to_short.clone().unwrap_or_default(),
            railway_to_code: c.railway_to_code,
            railway_part_to: c.railway_part_to.clone(),
            car_type:        car_type.clone(),
            etsng:           c.etsng.clone(),
            etsng_name:      c.etsng_name.clone(),
            needs_repair:    repair == RepairStatus::NeedsRepair,
            status:          c.grpo_name.clone(),
        };

        let data = groups.entry(key.clone()).or_insert_with(|| {
            key_order.push(key);
            GroupData::new()
        });

        data.car_count += 1;
        data.car_numbers.push(c.car_number);
        if let Some(v) = c.station_from      { data.stations_from.push(v); }
        if let Some(v) = c.station_from_code { data.stations_from_code.push(v); }
        if let Some(v) = c.railway_from_short { data.railways_from.push(v); }
        if let Some(v) = c.railway_from_code { data.railways_from_code.push(v); }
        if let Some(v) = c.railway_part_from { data.railways_part_from.push(v); }
        if let Some(v) = c.prev_etsng       { data.prev_etsngs.push(v); }
        if let Some(v) = c.prev_etsng_name  { data.prev_etsng_names.push(v); }
    }

    // --- Безномерные вагоны ---
    for c in no_number {
        let key = GroupKey {
            kind_ord:        kind_to_ord(&CarKind::NoNumber),
            supply_period,
            station_to:      c.station_to.clone().unwrap_or_default(),
            station_to_code: c.station_to_code.clone().unwrap_or_default(),
            railway_to:      c.railway_to_short.clone().unwrap_or_default(),
            railway_to_code: c.railway_to_code,
            railway_part_to: c.railway_part_to.clone(),
            car_type:        None,
            etsng:           c.etsng.clone(),
            etsng_name:      c.etsng_name.clone(),
            needs_repair:    false,
            status:          None,
        };

        let data = groups.entry(key.clone()).or_insert_with(|| {
            key_order.push(key);
            GroupData::new()
        });

        data.car_count += c.car_count;
    }

    // --- Сборка финальных узлов ---
    key_order
        .into_iter()
        .enumerate()
        .map(|(i, key)| {
            let data = groups.remove(&key).unwrap();
            SupplyNode {
                s_id:            i + 1,
                kind:            kind_from_ord(key.kind_ord),
                car_count:       data.car_count,
                station_to:      key.station_to,
                station_to_code: key.station_to_code,
                railway_to:      key.railway_to,
                railway_to_code: key.railway_to_code,
                railway_part_to: key.railway_part_to,
                car_type:        key.car_type,
                etsng:           key.etsng,
                etsng_name:      key.etsng_name,
                repair_status:   if key.needs_repair { RepairStatus::NeedsRepair } else { RepairStatus::Ok },
                status:          key.status,
                supply_period:   key.supply_period,
                car_numbers:        data.car_numbers,
                stations_from:      data.stations_from,
                stations_from_code: data.stations_from_code,
                railways_from:      data.railways_from,
                railways_from_code: data.railways_from_code,
                railways_part_from: data.railways_part_from,
                prev_etsngs:        data.prev_etsngs,
                prev_etsng_names:   data.prev_etsng_names,
                is_mass_unloading:  false, // заполняется ниже в mark_mass_unloading()
            }
        })
        .collect()
}

/// Убирает повторы номерных вагонов по `CarNumber` (остаётся первое вхождение).
///
/// Внутри одного источника вагон должен встречаться один раз: в АПИ — в одной дороге,
/// в дислокации — одним ключом HASH `supply_data`. Повторы возможны из-за JOIN'ов в
/// `dislocations.py` (`NSI.FrETSNG` по имени, `dynamic.CarComment` по `CarId`) или
/// аномалий выгрузки; без снятия дубля вагон учитывался бы в предложении дважды.
fn dedup_numbered_by_car_number(items: Vec<NumberedCarItem>) -> (Vec<NumberedCarItem>, usize) {
    let before = items.len();
    let mut seen: HashSet<u64> = HashSet::with_capacity(before);
    let items: Vec<NumberedCarItem> = items.into_iter().filter(|c| seen.insert(c.car_number)).collect();
    let duplicates = before - items.len();
    (items, duplicates)
}

/// Узлы дислокации (период 10) вместе со статистикой сверки номеров.
#[derive(Debug, Default)]
pub struct DislocationSupply {
    /// Узлы предложения `supply_period = 10` после снятия дублей и пересечения с периодом 1.
    pub nodes: Vec<SupplyNode>,
    /// Вагонов в JSON `dislocations.py` до проверок.
    pub cars_total: usize,
    /// Повторов номера внутри выгрузки дислокации (оставлено первое вхождение).
    pub duplicates_within: usize,
    /// Номера, уже присутствующие в предложении периода 1 (АПИ) — из периода 10 исключены.
    /// Сегодняшняя дислокация из АПИ точнее прогноза на 2–10 сутки, поэтому приоритет у неё.
    pub overlap_with_period1: Vec<u64>,
}

impl DislocationSupply {
    /// Вагонов, вошедших в узлы периода 10.
    pub fn cars_kept(&self) -> i32 {
        self.nodes.iter().map(|n| n.car_count).sum()
    }
}

/// Узлы предложения из JSON, который печатает `dislocations.py`
/// (массив объектов в формате полей `NumberedCarItem` из АПИ).
///
/// Период предложения `supply_period = 10` (2–10 сутки). `period1_cars` — номера
/// вагонов, уже вошедших в предложение периода 1 из АПИ: такие вагоны из дислокации
/// исключаются, чтобы один вагон не участвовал в оптимизации дважды. Повторы номера
/// внутри самой выгрузки схлопываются. Фильтрация идёт по записям **до** группировки,
/// иначе разошлись бы выровненные по вагонам списки узла (`car_numbers`, `stations_from*`).
/// `rules` — правило 7 при группировке (горизонт вывода в ремонт).
pub fn supply_nodes_from_dislocation_json(
    json: &str,
    period1_cars: &HashSet<u64>,
    rules: &BusinessRules,
) -> Result<DislocationSupply, serde_json::Error> {
    let numbered: Vec<NumberedCarItem> = serde_json::from_str(json)?;
    let cars_total = numbered.len();
    let (numbered, duplicates_within) = dedup_numbered_by_car_number(numbered);

    let mut overlap_with_period1: Vec<u64> = Vec::new();
    let numbered: Vec<NumberedCarItem> = numbered
        .into_iter()
        .filter(|c| {
            let overlaps = period1_cars.contains(&c.car_number);
            if overlaps {
                overlap_with_period1.push(c.car_number);
            }
            !overlaps
        })
        .collect();

    let nodes = group_supply(numbered.into_iter(), std::iter::empty::<NoNumberItem>(), 10, rules);
    Ok(DislocationSupply { nodes, cars_total, duplicates_within, overlap_with_period1 })
}

/// Помечает узлы предложения, относящиеся к станциям массовой выгрузки.
///
/// Станция считается массовой, если суммарное количество вагонов по всем
/// узлам с одним `station_to_code` превышает [`MASS_UNLOADING_THRESHOLD`].
pub fn apply_mass_unloading_flags(nodes: &mut [SupplyNode]) {
    // Шаг 1: суммируем car_count по station_to_code (владеющие ключи — нет borrow-конфликта).
    let mut sums: HashMap<String, i32> = HashMap::new();
    for node in nodes.iter() {
        *sums.entry(node.station_to_code.clone()).or_insert(0) += node.car_count;
    }

    // Шаг 2: устанавливаем флаг.
    for node in nodes.iter_mut() {
        node.is_mass_unloading =
            sums.get(&node.station_to_code).copied().unwrap_or(0)
                > MASS_UNLOADING_THRESHOLD;
    }
}

// ---------------------------------------------------------------------------
// Методы ApiClient
// ---------------------------------------------------------------------------

impl ApiClient {
    pub async fn fetch_supply_nodes(&self, rules: &BusinessRules) -> Result<Vec<SupplyNode>, ApiError> {
        let doc_date = Utc::now().format("%Y-%m-%d").to_string();
        // let doc_date = "2026-06-10".to_string(); // TEMP: фиксированная дата для теста в выходной день
        // let doc_date = chrono::NaiveDate::from_ymd_opt(2026, 6, 10).unwrap();
        let url = ApiEndpoint::Supply.url(&self.base_url);

        let response = self
            .client
            .get(&url)
            .query(&[("docDate", &doc_date)])
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::UnexpectedStatus { status: status.as_u16(), body });
        }

        let railway_items = response.json::<Vec<SupplyApiItem>>().await?;

        let mut numbered_all:  Vec<NumberedCarItem> = Vec::new();
        let mut no_number_all: Vec<NoNumberItem>    = Vec::new();

        for item in railway_items {
            numbered_all.extend(item.numbered);
            no_number_all.extend(item.no_number);
        }

        // Один вагон — одна запись: повтор номера между дорогами ответа удвоил бы предложение.
        let (numbered_all, duplicates) = dedup_numbered_by_car_number(numbered_all);
        if duplicates > 0 {
            eprintln!(
                "  [!] АПИ предложения: {duplicates} повторов номеров вагонов в opzCarNumberModelCollection — оставлено первое вхождение"
            );
        }

        Ok(group_supply(
            numbered_all.into_iter(),
            no_number_all.into_iter(),
            1,
            rules,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Запись дислокации в формате `dislocations.py`: только поля, влияющие на ключ группы.
    fn car_json(car_number: u64, station_to_code: &str, station_from_code: &str) -> String {
        format!(
            r#"{{"CarNumber": {car_number}, "StationTo": "СТ-{station_to_code}", "StationToCode": "{station_to_code}",
                "RailWayToShort": "СКВ", "StationFromCode": "{station_from_code}", "OPZRailWayId": null,
                "OPZComment1": "БКТ", "GRPOName": "ПОР", "PrevFrETSNGCode": "011005"}}"#
        )
    }

    fn json_of(cars: &[String]) -> String {
        format!("[{}]", cars.join(","))
    }

    fn disl(json: &str, period1: &HashSet<u64>) -> DislocationSupply {
        supply_nodes_from_dislocation_json(json, period1, &BusinessRules::default()).unwrap()
    }

    fn all_car_numbers(nodes: &[SupplyNode]) -> Vec<u64> {
        let mut v: Vec<u64> = nodes.iter().flat_map(|n| n.car_numbers.iter().copied()).collect();
        v.sort_unstable();
        v
    }

    /// Вагоны, уже вошедшие в период 1 (АПИ), из дислокации исключаются; остальные — узлы периода 10.
    #[test]
    fn dislocation_drops_cars_already_in_period1() {
        let json = json_of(&[
            car_json(1001, "100001", "200001"),
            car_json(1002, "100001", "200002"),
            car_json(1003, "100002", "200003"),
        ]);
        let period1: HashSet<u64> = [1002_u64, 9999].into_iter().collect();

        let d = disl(&json, &period1);
        assert_eq!(d.cars_total, 3);
        assert_eq!(d.duplicates_within, 0);
        assert_eq!(d.overlap_with_period1, vec![1002]);
        assert_eq!(d.cars_kept(), 2);
        assert_eq!(all_car_numbers(&d.nodes), vec![1001, 1003]);
        assert!(d.nodes.iter().all(|n| n.supply_period == 10));
        // Выровненные по вагонам списки узла не расходятся после фильтрации.
        for n in &d.nodes {
            assert_eq!(n.car_numbers.len(), n.stations_from_code.len());
            assert_eq!(n.car_count as usize, n.car_numbers.len());
        }
        let node_100001 = d.nodes.iter().find(|n| n.station_to_code == "100001").unwrap();
        assert_eq!(node_100001.stations_from_code, vec!["200001"], "станция отправления вагона 1002 не должна остаться");
    }

    /// Повтор номера внутри выгрузки (размножение строк JOIN'ами в dislocations.py) схлопывается,
    /// остаётся первое вхождение; пересечение с периодом 1 считается по уникальным номерам.
    #[test]
    fn dislocation_dedups_repeated_car_numbers_within_dump() {
        let json = json_of(&[
            car_json(1001, "100001", "200001"),
            car_json(1001, "100001", "200001"),
            car_json(1001, "100002", "200009"),
            car_json(1002, "100001", "200002"),
            car_json(1002, "100001", "200002"),
        ]);
        let period1: HashSet<u64> = [1002_u64].into_iter().collect();

        let d = disl(&json, &period1);
        assert_eq!(d.cars_total, 5);
        assert_eq!(d.duplicates_within, 3);
        assert_eq!(d.overlap_with_period1, vec![1002]);
        assert_eq!(d.cars_kept(), 1);
        assert_eq!(all_car_numbers(&d.nodes), vec![1001]);
        assert_eq!(d.nodes[0].station_to_code, "100001", "первое вхождение вагона 1001");
    }

    /// Без пересечений и дублей выгрузка проходит как есть; пустой набор периода 1 ничего не режет.
    #[test]
    fn dislocation_without_overlap_is_unchanged() {
        let json = json_of(&[car_json(1001, "100001", "200001"), car_json(1002, "100001", "200002")]);
        let d = disl(&json, &HashSet::new());
        assert_eq!(d.cars_total, 2);
        assert_eq!(d.duplicates_within, 0);
        assert!(d.overlap_with_period1.is_empty());
        assert_eq!(d.cars_kept(), 2);
        assert_eq!(d.nodes.len(), 1, "одна группа: станция, тип, ЕТСНГ и статус совпадают");
        assert_eq!(d.nodes[0].car_numbers, vec![1001, 1002]);
    }

    /// Все вагоны выгрузки уже в периоде 1 — узлов периода 10 нет, статистика заполнена.
    #[test]
    fn dislocation_fully_covered_by_period1_yields_no_nodes() {
        let json = json_of(&[car_json(1001, "100001", "200001")]);
        let period1: HashSet<u64> = [1001_u64].into_iter().collect();
        let d = disl(&json, &period1);
        assert!(d.nodes.is_empty());
        assert_eq!(d.cars_total, 1);
        assert_eq!(d.overlap_with_period1, vec![1001]);
        assert_eq!(d.cars_kept(), 0);
    }

    /// Дедупликация номерных записей АПИ: повтор номера между дорогами ответа — одно вхождение.
    #[test]
    fn api_numbered_items_dedup_keeps_first() {
        let items: Vec<NumberedCarItem> = serde_json::from_str(&json_of(&[
            car_json(1001, "100001", "200001"),
            car_json(1002, "100001", "200002"),
            car_json(1001, "100003", "200003"),
        ]))
        .unwrap();
        let (items, dups) = dedup_numbered_by_car_number(items);
        assert_eq!(dups, 1);
        let nums: Vec<u64> = items.iter().map(|c| c.car_number).collect();
        assert_eq!(nums, vec![1001, 1002]);
        assert_eq!(items[0].station_to_code.as_deref(), Some("100001"), "оставлено первое вхождение");
    }

    fn car_json_repair(car_number: u64, railway_to: &str, days: f64, is_car_repair: bool) -> String {
        format!(
            r#"{{"CarNumber": {car_number}, "StationTo": "СТ", "StationToCode": "100001",
                "RailWayToShort": "{railway_to}", "StationFromCode": "200001", "OPZRailWayId": null,
                "OPZComment1": "БКТ", "GRPOName": "ПОР", "PrevFrETSNGCode": "011005",
                "CarNextRepairDays": {days}, "IsCarRepair": {is_car_repair}}}"#
        )
    }

    fn rules_with_foreign() -> BusinessRules {
        let mut r = BusinessRules::default();
        r.foreign_railways = ["КЗХ", "УЗБ"].iter().map(|s| s.to_string()).collect();
        r
    }

    /// Правило 7: на российской дороге порог 15 сут., на инотерритории 45; IsCarRepair безусловен.
    #[test]
    fn grouping_marks_repair_by_rule7_thresholds() {
        let json = json_of(&[
            car_json_repair(1, "СКВ", 14.0, false),
            car_json_repair(2, "СКВ", 20.0, false),
            car_json_repair(3, "КЗХ", 20.0, false),
            car_json_repair(4, "КЗХ", 45.0, false),
            car_json_repair(5, "УЗБ", 100.0, true),
        ]);
        let d = supply_nodes_from_dislocation_json(&json, &HashSet::new(), &rules_with_foreign()).unwrap();
        let status = |n: u64| {
            d.nodes.iter().find(|node| node.car_numbers.contains(&n)).map(|node| node.repair_status.clone())
        };
        assert_eq!(status(1), Some(RepairStatus::NeedsRepair), "СКВ 14 < 15");
        assert_eq!(status(2), Some(RepairStatus::Ok), "СКВ 20 ≥ 15");
        assert_eq!(status(3), Some(RepairStatus::NeedsRepair), "КЗХ 20 < 45 — вывоз с инотерритории");
        assert_eq!(status(4), Some(RepairStatus::Ok), "КЗХ 45 ≥ 45");
        assert_eq!(status(5), Some(RepairStatus::NeedsRepair), "IsCarRepair");
        // Ремонт входит в ключ группы: СКВ 14 и СКВ 20 — разные узлы.
        assert_eq!(
            d.nodes.iter().filter(|n| n.railway_to == "СКВ").count(),
            2
        );
    }
}

use std::collections::HashMap;

use chrono::Utc;
use serde::Deserialize;

use crate::node::{CarKind, RepairStatus, SupplyNode};
use super::client::{ApiClient, ApiEndpoint, ApiError};

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

/// Именной вагон из `opzCarNumberModelCollection`.
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
}

impl NumberedCarItem {
    fn car_type(&self) -> Option<String> {
        self.opz_c1.as_deref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    fn repair_status(&self) -> RepairStatus {
        let needs = self.is_car_repair
            || self.days_to_repair.map(|d| d < 15.0).unwrap_or(false);
        if needs { RepairStatus::NeedsRepair } else { RepairStatus::Ok }
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
fn group_supply(
    numbered: impl Iterator<Item = NumberedCarItem>,
    no_number: impl Iterator<Item = NoNumberItem>,
) -> Vec<SupplyNode> {
    let mut groups: HashMap<GroupKey, GroupData> = HashMap::new();
    // Сохраняем порядок первого появления ключа.
    let mut key_order: Vec<GroupKey> = Vec::new();

    // --- Именные вагоны ---
    for c in numbered {
        let car_type = c.car_type();
        let repair   = c.repair_status();
        let kind     = if c.opz_railway_id.is_some() { CarKind::Assigned } else { CarKind::Free };

        let key = GroupKey {
            kind_ord:        kind_to_ord(&kind),
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
                car_numbers:        data.car_numbers,
                stations_from:      data.stations_from,
                stations_from_code: data.stations_from_code,
                railways_from:      data.railways_from,
                railways_from_code: data.railways_from_code,
                railways_part_from: data.railways_part_from,
                prev_etsngs:        data.prev_etsngs,
                prev_etsng_names:   data.prev_etsng_names,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Методы ApiClient
// ---------------------------------------------------------------------------

impl ApiClient {
    pub async fn fetch_supply_nodes(&self) -> Result<Vec<SupplyNode>, ApiError> {
        let doc_date = Utc::now().format("%Y-%m-%d").to_string();
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

        Ok(group_supply(numbered_all.into_iter(), no_number_all.into_iter()))
    }
}

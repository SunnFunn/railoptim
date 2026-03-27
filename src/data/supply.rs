use chrono::Utc;
use serde::Deserialize;

use crate::node::{CarKind, SupplyNode};
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

    // Характеристики
    #[serde(rename = "CarCapacity",   default)] capacity:  f64,
    #[serde(rename = "CarBodyVolume", default)] volume:    f64,
    #[serde(rename = "CarModel",      default)] car_model: Option<String>,

    // Груз
    #[serde(rename = "GRPOName",          default)] grpo_name:       Option<String>,
    #[serde(rename = "FrETSNGCode",       default)] etsng:           Option<String>,
    #[serde(rename = "FrETSNGName",       default)] etsng_name:      Option<String>,
    #[serde(rename = "PrevFrETSNGCode",   default)] prev_etsng:      Option<String>,
    #[serde(rename = "PrevFrETSNGName",   default)] prev_etsng_name: Option<String>,

    // Ремонт
    #[serde(rename = "CarNextRepairDays",     default)] days_to_repair: Option<f64>,
    #[serde(rename = "CarNextRepairTypeName", default)] repair_type:    Option<String>,

    // Комментарии (OPZComment1 = тип вагона, 2–10 = прочие)
    #[serde(rename = "CarCommentODO",  default)] comment_odo:  Option<String>,
    #[serde(rename = "CarCommentODO2", default)] comment_odo2: Option<String>,
    #[serde(rename = "OPZComment1",  default)] opz_c1:  Option<String>,
    #[serde(rename = "OPZComment2",  default)] opz_c2:  Option<String>,
    #[serde(rename = "OPZComment3",  default)] opz_c3:  Option<String>,
    #[serde(rename = "OPZComment4",  default)] opz_c4:  Option<String>,
    #[serde(rename = "OPZComment5",  default)] opz_c5:  Option<String>,
    #[serde(rename = "OPZComment6",  default)] opz_c6:  Option<String>,
    #[serde(rename = "OPZComment7",  default)] opz_c7:  Option<String>,
    #[serde(rename = "OPZComment8",  default)] opz_c8:  Option<String>,
    #[serde(rename = "OPZComment9",  default)] opz_c9:  Option<String>,
    #[serde(rename = "OPZComment10", default)] opz_c10: Option<String>,

    // Прочее
    #[serde(rename = "NextClaim", default)] next_claim: Option<String>,
    #[serde(rename = "IdleTime",  default)] idle_time:  Option<f64>,
}

impl NumberedCarItem {
    /// Тип вагона — берём из OPZComment1 (trimmed).
    fn car_type(&self) -> Option<String> {
        self.opz_c1.as_deref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    /// Непустые OPZComment2..10, объединённые через " | ".
    fn opz_comments(&self) -> Option<String> {
        let parts: Vec<&str> = [
            &self.opz_c2, &self.opz_c3, &self.opz_c4, &self.opz_c5,
            &self.opz_c6, &self.opz_c7, &self.opz_c8, &self.opz_c9, &self.opz_c10,
        ]
        .iter()
        .filter_map(|o| o.as_deref())
        .filter(|s| !s.trim().is_empty())
        .collect();

        if parts.is_empty() { None } else { Some(parts.join(" | ")) }
    }

    fn into_supply_node(self, id: usize, kind: CarKind) -> SupplyNode {
        let car_type     = self.car_type();
        let opz_comments = self.opz_comments();
        SupplyNode {
            s_id: id,
            kind,
            car_number:        Some(self.car_number),
            car_count:         1,
            station_from:      self.station_from,
            station_from_code: self.station_from_code,
            railway_from:      self.railway_from_short,
            railway_from_code: self.railway_from_code,
            railway_part_from: self.railway_part_from,
            station_to:        self.station_to.unwrap_or_default(),
            station_to_code:   self.station_to_code.unwrap_or_default(),
            railway_to:        self.railway_to_short.unwrap_or_default(),
            railway_to_code:   self.railway_to_code,
            railway_part_to:   self.railway_part_to,
            capacity:          self.capacity,
            volume:            self.volume,
            car_type,
            car_model:         self.car_model,
            status:            self.grpo_name,
            etsng:             self.etsng,
            etsng_name:        self.etsng_name,
            prev_etsng:        self.prev_etsng,
            prev_etsng_name:   self.prev_etsng_name,
            days_to_repair:    self.days_to_repair,
            repair_type:       self.repair_type,
            comment_odo:       self.comment_odo,
            comment_odo2:      self.comment_odo2,
            opz_comments,
            next_claim:        self.next_claim,
            idle_time:         self.idle_time,
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
    #[serde(rename = "CarCount",       default)] car_count:        i32,
}

impl NoNumberItem {
    fn into_supply_node(self, id: usize) -> SupplyNode {
        SupplyNode {
            s_id: id,
            kind: CarKind::NoNumber,
            car_number:        None,
            car_count:         self.car_count,
            station_from:      None,
            station_from_code: None,
            railway_from:      None,
            railway_from_code: None,
            railway_part_from: None,
            station_to:        self.station_to.unwrap_or_default(),
            station_to_code:   self.station_to_code.unwrap_or_default(),
            railway_to:        self.railway_to_short.unwrap_or_default(),
            railway_to_code:   self.railway_to_code,
            railway_part_to:   self.railway_part_to,
            capacity:          0.0,
            volume:            0.0,
            car_type:          None,
            car_model:         None,
            status:            None,
            etsng:             None,
            etsng_name:        None,
            prev_etsng:        None,
            prev_etsng_name:   None,
            days_to_repair:    None,
            repair_type:       None,
            comment_odo:       None,
            comment_odo2:      None,
            opz_comments:      None,
            next_claim:        None,
            idle_time:         None,
        }
    }
}

// ---------------------------------------------------------------------------
// Методы ApiClient
// ---------------------------------------------------------------------------

impl ApiClient {
    /// Запрашивает узлы предложения порожних вагонов на текущую дату.
    ///
    /// API возвращает массив объектов (по одному на дорогу). Каждый объект
    /// содержит две группы: именные вагоны (свободные и по факту) и безномерные.
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

        enum CarEntry {
            Numbered(CarKind, NumberedCarItem),
            NoNumber(NoNumberItem),
        }

        let nodes = railway_items
            .into_iter()
            .flat_map(|item| {
                let numbered = item.numbered.into_iter().map(|c| {
                    let kind = if c.opz_railway_id.is_some() { CarKind::Assigned } else { CarKind::Free };
                    CarEntry::Numbered(kind, c)
                });
                let no_number = item.no_number.into_iter().map(CarEntry::NoNumber);
                numbered.chain(no_number)
            })
            .enumerate()
            .map(|(i, entry)| match entry {
                CarEntry::Numbered(kind, c) => c.into_supply_node(i + 1, kind),
                CarEntry::NoNumber(c)       => c.into_supply_node(i + 1),
            })
            .collect();

        Ok(nodes)
    }
}

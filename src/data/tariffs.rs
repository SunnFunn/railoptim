use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::node::TariffNode;
use super::client::{ApiClient, ApiEndpoint, ApiError};

// ---------------------------------------------------------------------------
// Структуры запроса
// ---------------------------------------------------------------------------

/// Ссылка на одну станцию для тела POST-запроса.
#[derive(Serialize, Debug, Clone)]
pub struct StationRef {
    #[serde(rename = "StationCode")]
    pub station_code: String,
    #[serde(rename = "RailWayShortName")]
    pub railway_short_name: String,
}

impl StationRef {
    pub fn new(station_code: impl Into<String>, railway_short_name: impl Into<String>) -> Self {
        Self {
            station_code: station_code.into(),
            railway_short_name: railway_short_name.into(),
        }
    }
}

/// Тело POST-запроса к `GetRailTariffRouteDataTransmission`.
#[derive(Serialize, Debug)]
struct TariffRequest<'a> {
    #[serde(rename = "StationsFrom")]
    stations_from: &'a [StationRef],
    #[serde(rename = "StationsTo")]
    stations_to: &'a [StationRef],
}

// ---------------------------------------------------------------------------
// Структуры ответа
// ---------------------------------------------------------------------------

#[derive(Deserialize, Debug)]
struct TariffApiItem {
    #[serde(rename = "StationFrom")]     station_from:      String,
    #[serde(rename = "StationFromCode")] station_from_code: String,
    #[serde(rename = "RailWayFromName")] railway_from_name: String,
    #[serde(rename = "RailWayFromCode")] railway_from_code: i32,
    #[serde(rename = "StationTo")]       station_to:        String,
    #[serde(rename = "StationToCode")]   station_to_code:   String,
    #[serde(rename = "RailWayToName")]   railway_to_name:   String,
    #[serde(rename = "RailWayToCode")]   railway_to_code:   i32,
    #[serde(rename = "Distance")]        distance:          i32,
    #[serde(rename = "PeriodOfDelivery")] period_of_delivery: i32,
    #[serde(rename = "Cost")]            cost:              i64,
    #[serde(rename = "ActualDate")]      actual_date:       NaiveDateTime,
}

impl TariffApiItem {
    fn into_tariff_node(self) -> TariffNode {
        TariffNode {
            station_from:       self.station_from,
            station_from_code:  self.station_from_code,
            railway_from:       self.railway_from_name,
            railway_from_code:  self.railway_from_code,
            station_to:         self.station_to,
            station_to_code:    self.station_to_code,
            railway_to:         self.railway_to_name,
            railway_to_code:    self.railway_to_code,
            distance:           self.distance,
            period_of_delivery: self.period_of_delivery,
            cost:               self.cost,
            actual_date:        self.actual_date,
        }
    }
}

// ---------------------------------------------------------------------------
// Методы ApiClient
// ---------------------------------------------------------------------------

impl ApiClient {
    /// Запрашивает тарифы для всех пар станций отправления → назначения.
    ///
    /// Станции отправления обычно берутся из [`SupplyNode::station_to_code`],
    /// станции назначения — из [`DemandNode::station_code`].
    ///
    /// # Пример
    /// ```rust,ignore
    /// let from: Vec<StationRef> = supply_nodes.iter()
    ///     .map(|n| StationRef::new(&n.station_to_code, &n.railway_to))
    ///     .collect::<std::collections::HashSet<_>>() // дедупликация
    ///     .into_iter().collect();
    ///
    /// let to: Vec<StationRef> = demand_nodes.iter()
    ///     .map(|n| StationRef::new(&n.station_code, &n.railway_name))
    ///     .collect::<std::collections::HashSet<_>>()
    ///     .into_iter().collect();
    ///
    /// let tariffs = client.fetch_tariffs(&from, &to).await?;
    /// ```
    pub async fn fetch_tariffs(
        &self,
        stations_from: &[StationRef],
        stations_to: &[StationRef],
    ) -> Result<Vec<TariffNode>, ApiError> {
        let url = ApiEndpoint::Tariffs.url(&self.base_url);

        let body = TariffRequest { stations_from, stations_to };

        let response = self
            .client
            .post(&url)
            .json(&body)
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

        let items = response.json::<Vec<TariffApiItem>>().await?;
        Ok(items.into_iter().map(TariffApiItem::into_tariff_node).collect())
    }
}

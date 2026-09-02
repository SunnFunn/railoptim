use std::time::Duration;

use chrono::NaiveDateTime;
use reqwest::header;
use serde::{Deserialize, Serialize};

use crate::node::TariffNode;
use super::client::{ApiClient, ApiEndpoint, ApiError};

/// Сколько раз запрашивать тарифы при 502/обрыве соединения (включая первую попытку).
const TARIFF_FETCH_ATTEMPTS: u32 = 3;
/// Пауза между попытками: IIS/бэкенд после тяжёлого расчёта матрицы часто «отходит» не сразу.
const TARIFF_RETRY_PAUSE: Duration = Duration::from_secs(15);

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
    #[serde(rename = "Cost")]            cost:              f64,
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

        let body = TariffRequest {
            stations_from,
            stations_to,
        };

        for attempt in 1..=TARIFF_FETCH_ATTEMPTS {
            match self.fetch_tariffs_once(&url, &body).await {
                Ok(items) => {
                    if attempt > 1 {
                        eprintln!(
                            "  тарифы: успешно с попытки {attempt}/{TARIFF_FETCH_ATTEMPTS}"
                        );
                    }
                    return Ok(items);
                }
                Err(e) => {
                    let retry =
                        is_retryable_tariff_error(&e) && attempt < TARIFF_FETCH_ATTEMPTS;
                    if !retry {
                        return Err(e);
                    }
                    eprintln!(
                        "  тарифы: попытка {attempt}/{TARIFF_FETCH_ATTEMPTS} не удалась ({}) — повтор через {} с (новое соединение)",
                        short_api_error(&e),
                        TARIFF_RETRY_PAUSE.as_secs(),
                    );
                    tokio::time::sleep(TARIFF_RETRY_PAUSE).await;
                }
            }
        }
        unreachable!("цикл retry либо возвращает Ok, либо Err до исчерпания попыток");
    }

    /// Один POST тарифов. `Connection: close` — не оставлять keep-alive в пуле:
    /// после 502 IIS (и после тяжёлого успешного ответа) повторное использование
    /// того же TCP-соединения даёт `error sending request` на следующем вызове.
    async fn fetch_tariffs_once(
        &self,
        url: &str,
        body: &TariffRequest<'_>,
    ) -> Result<Vec<TariffNode>, ApiError> {
        let response = self
            .client
            .post(url)
            .header(header::CONNECTION, "close")
            .json(body)
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ApiError::UnexpectedStatus {
                status: status.as_u16(),
                body: truncate_error_body(&text),
            });
        }

        let items = response.json::<Vec<TariffApiItem>>().await?;
        Ok(items.into_iter().map(TariffApiItem::into_tariff_node).collect())
    }
}

/// Повторяем при шлюзовых сбоях IIS и обрыве на отправке; 401 и 4xx клиента — нет.
fn is_retryable_tariff_error(err: &ApiError) -> bool {
    match err {
        ApiError::Unauthorized => false,
        ApiError::UnexpectedStatus { status, .. } => matches!(*status, 408 | 429 | 502 | 503 | 504),
        ApiError::Http(e) => e.is_connect() || e.is_timeout() || e.is_request() || e.is_body(),
    }
}

fn truncate_error_body(body: &str) -> String {
    const MAX: usize = 200;
    let one_line: String = body
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let collapsed = one_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX {
        collapsed
    } else {
        let short: String = collapsed.chars().take(MAX).collect();
        format!("{short}…")
    }
}

fn short_api_error(err: &ApiError) -> String {
    match err {
        ApiError::UnexpectedStatus { status, body } => format!("HTTP {status}: {body}"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_gateway_and_rate_limit() {
        for status in [408, 429, 502, 503, 504] {
            let err = ApiError::UnexpectedStatus {
                status,
                body: "gateway".into(),
            };
            assert!(is_retryable_tariff_error(&err), "status {status}");
        }
    }

    #[test]
    fn does_not_retry_auth_or_client_errors() {
        assert!(!is_retryable_tariff_error(&ApiError::Unauthorized));
        assert!(!is_retryable_tariff_error(&ApiError::UnexpectedStatus {
            status: 400,
            body: "bad".into(),
        }));
        assert!(!is_retryable_tariff_error(&ApiError::UnexpectedStatus {
            status: 404,
            body: "no".into(),
        }));
    }

    #[test]
    fn truncates_iis_html() {
        let html = "<!DOCTYPE html>".to_string() + &"<p>502 шлюз</p>".repeat(40);
        let t = truncate_error_body(&html);
        assert!(t.chars().count() <= 201); // 200 + возможное «…»
        assert!(!t.contains('\n'));
    }
}

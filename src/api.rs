use chrono::{Duration, TimeZone, Utc};
use reqwest::{
    header::{self, HeaderMap, HeaderValue},
    Client,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use thiserror::Error;
use zeroize::Zeroize;

use crate::node::DemandNode;

/// Формат дат в параметрах запроса к API.
const DATE_FMT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";

/// Периоды планирования спроса: (смещение начала, смещение конца) в сутках от даты запроса.
///
/// - Период 1: сутки  1–5
/// - Период 2: сутки  6–8
/// - Период 3: сутки  9–10
/// - Период 4: сутки 11–15
const DEMAND_PERIODS: [(i64, i64); 4] = [(1, 5), (6, 8), (9, 10), (11, 15)];

// ---------------------------------------------------------------------------
// Ошибки
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("HTTP-запрос завершился ошибкой: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Неверный или просроченный токен (401 Unauthorized)")]
    Unauthorized,

    #[error("Неожиданный статус ответа {status}: {body}")]
    UnexpectedStatus { status: u16, body: String },
}

// ---------------------------------------------------------------------------
// Эндпойнты
// ---------------------------------------------------------------------------

/// Эндпойнты API.
///
/// Базовый URL: `https://isupv-api.rusagrotrans.ru:2555/isupv/IsupvApi`
pub enum ApiEndpoint {
    Demand,
    Supply,
    Tariffs,
    Output,
}

impl ApiEndpoint {
    /// Путь эндпойнта (без базового URL).
    pub fn path(&self) -> &'static str {
        match self {
            Self::Demand  => "GetDemandDataTransmission",
            Self::Supply  => "GetSupplyDataTransmission",
            Self::Tariffs => "GetRailTariffRouteDataTransmission",
            Self::Output  => "DestinationRegistryTransmission",
        }
    }

    /// Полный URL эндпойнта.
    pub fn url(&self, base_url: &str) -> String {
        format!("{}/{}", base_url, self.path())
    }
}

// ---------------------------------------------------------------------------
// Внутренние структуры десериализации ответа API
// ---------------------------------------------------------------------------

/// Обёртка ответа GetDemandDataTransmission. Поле `meta` игнорируется serde.
#[derive(Deserialize, Debug)]
struct DemandApiResponse {
    data: Vec<DemandApiItem>,
}

/// Одна запись из `data[]` ответа GetDemandDataTransmission.
#[derive(Deserialize, Debug)]
struct DemandApiItem {
    // Период погрузки
    #[serde(rename = "LoadDateStart")]
    load_date_start: String,
    #[serde(rename = "LoadDateEnd")]
    load_date_end: String,

    // Дорога и станция погрузки (From)
    #[serde(rename = "RailWayPartFrom",  default)] rail_way_part_from:  Option<String>,
    #[serde(rename = "RailWayShortFrom", default)] rail_way_short_from: Option<String>,
    #[serde(rename = "RailWayFromCode",  default)] rail_way_from_code:  Option<String>,
    #[serde(rename = "StationFrom",      default)] station_from:        Option<String>,
    #[serde(rename = "StationFromCode",  default)] station_from_code:   Option<String>,

    // Дорога и станция назначения (To)
    #[serde(rename = "RailWayPartTo",  default)] rail_way_part_to:  Option<String>,
    #[serde(rename = "RailWayShortTo", default)] rail_way_short_to: Option<String>,
    #[serde(rename = "RailWayToCode",  default)] rail_way_to_code:  Option<String>,
    #[serde(rename = "StationTo",      default)] station_to:        Option<String>,
    #[serde(rename = "StationToCode",  default)] station_to_code:   Option<String>,

    // Грузоотправитель
    #[serde(rename = "Sender",    default)] sender:     Option<String>,
    #[serde(rename = "SenderOKPO", default)] sender_okpo: Option<String>,
    #[serde(rename = "SenderTGNL", default)] sender_tgnl: Option<String>,

    // Клиент и грузополучатель
    #[serde(rename = "Client",       default)] client:        Option<Vec<String>>,
    #[serde(rename = "CustomerOKPO", default)] customer_okpo: Option<Vec<String>>,
    #[serde(rename = "Recip",        default)] recip:         Option<Vec<String>>,
    #[serde(rename = "LoaderToOKPO", default)] loader_to_okpo: Option<Vec<String>>,

    // Груз
    #[serde(rename = "NameGNG",      default)] name_gng:     Option<String>,
    #[serde(rename = "FrETSNGCode",  default)] fr_etsng_code: Option<String>,

    // Заявки
    #[serde(rename = "DocumentNumber", default)] document_number: Option<Vec<String>>,
    #[serde(rename = "DocumentDate",   default)] document_date:   Option<Vec<String>>,
    #[serde(rename = "GU12Number",     default)] gu12_number:     Option<Vec<String>>,

    // Тип отправки
    #[serde(rename = "LoadTypeName", default)] load_type_name: Option<String>,

    // Количество вагонов и вес
    #[serde(rename = "PlannedCarsToLoad",   default)] planned_cars_to_load:   i32,
    #[serde(rename = "PlannedWeightToLoad", default)] planned_weight_to_load: f64,
    #[serde(rename = "ProvidedCarsToLoad",  default)] provided_cars_to_load:  i32,
    #[serde(rename = "CarsOnStation",       default)] cars_on_station:        i32,
}

impl DemandApiItem {
    /// Конвертирует запись API в [`DemandNode`].
    ///
    /// - `id` — сквозной порядковый номер узла.
    /// - `period` — номер планового периода: 1 (сут. 1–5), 2 (6–8), 3 (9–10), 4 (11–15).
    fn into_demand_node(self, id: usize, period: u8) -> DemandNode {
        // Потребность в вагонах = план − уже обеспечено (не меньше 0).
        let car_count = (self.planned_cars_to_load - self.provided_cars_to_load).max(0);

        // Тип вагона по удельной нагрузке: > 70 т/ваг → БКТ, иначе → Прочие.
        let car_type = if self.planned_cars_to_load > 0 {
            let weight_per_car = self.planned_weight_to_load / self.planned_cars_to_load as f64;
            Some(if weight_per_car > 70.0 { "БКТ" } else { "Прочие" }.to_string())
        } else {
            None
        };

        DemandNode {
            d_id: id,
            period,
            station_name:      self.station_from.unwrap_or_default(),
            station_code:      self.station_from_code.unwrap_or_default(),
            railway_name:      self.rail_way_short_from.unwrap_or_default(),
            railway_code:      self.rail_way_from_code,
            railway_part:      self.rail_way_part_from,
            station_to_name:   self.station_to,
            station_to_code:   self.station_to_code,
            railway_to_name:   self.rail_way_short_to,
            railway_to_code:   self.rail_way_to_code,
            railway_to_part:   self.rail_way_part_to,
            sender:            self.sender,
            sender_okpo:       self.sender_okpo,
            sender_tgnl:       self.sender_tgnl,
            client:            self.client,
            customer_okpo:     self.customer_okpo,
            recipient:         self.recip,
            loader_to_okpo:    self.loader_to_okpo,
            gng_cargo:         self.name_gng,
            etsng:             self.fr_etsng_code,
            request_numbers:   self.document_number,
            request_dates:     self.document_date,
            gu12_number:       self.gu12_number,
            shipping_type:     self.load_type_name,
            car_type,
            car_count,
            cars_on_station:   self.cars_on_station,
        }
    }
}


// ---------------------------------------------------------------------------
// API-клиент
// ---------------------------------------------------------------------------

/// Клиент для работы с API данных оптимизации.
pub struct ApiClient {
    client: Client,
    base_url: String,
}

impl ApiClient {
    /// Создаёт новый экземпляр клиента с Bearer-токеном.
    ///
    /// Токен передаётся как [`SecretString`] и раскрывается только на время
    /// формирования заголовка; временная строка затирается через [`zeroize`].
    pub fn new(base_url: impl Into<String>, token: &SecretString) -> Result<Self, ApiError> {
        let mut bearer = format!("Bearer {}", token.expose_secret());

        let mut auth_value = HeaderValue::from_str(&bearer).map_err(|_| {
            ApiError::UnexpectedStatus {
                status: 0,
                body: "Токен содержит недопустимые символы".to_string(),
            }
        })?;

        bearer.zeroize();

        auth_value.set_sensitive(true);

        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, auth_value);

        let client = Client::builder()
            .default_headers(headers)
            .build()
            .map_err(ApiError::Http)?;

        Ok(Self {
            client,
            base_url: base_url.into(),
        })
    }

    /// Один GET-запрос к эндпойнту Demand для конкретного временного окна.
    /// Возвращает сырые записи [`DemandApiItem`] вместе с номером периода.
    async fn fetch_demand_period(
        &self,
        date_start: &str,
        date_end: &str,
        period: u8,
    ) -> Result<(u8, Vec<DemandApiItem>), ApiError> {
        let url = ApiEndpoint::Demand.url(&self.base_url);

        let response = self
            .client
            .get(&url)
            .query(&[
                ("LoadDateStart", date_start),
                ("LoadDateEnd",   date_end),
                ("Page",          "1"),
                ("PageSize",      "10000"),
            ])
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::UnexpectedStatus {
                status: status.as_u16(),
                body,
            });
        }

        let resp = response.json::<DemandApiResponse>().await?;
        Ok((period, resp.data))
    }

    /// Запрашивает все узлы спроса по четырём периодам планирования параллельно.
    ///
    /// Четыре запроса выполняются одновременно; результаты объединяются в один
    /// список с присвоением сквозных ID.
    pub async fn fetch_demand_nodes(&self) -> Result<Vec<DemandNode>, ApiError> {
        let today = Utc::now().date_naive();

        let periods: Vec<(String, String)> = DEMAND_PERIODS
            .iter()
            .map(|&(start, end)| {
                let fmt_day = |offset: i64, hh: u32, mm: u32, ss: u32, ms: u32| {
                    let naive = (today + Duration::days(offset))
                        .and_hms_milli_opt(hh, mm, ss, ms)
                        .expect("корректное время суток");
                    Utc.from_utc_datetime(&naive).format(DATE_FMT).to_string()
                };
                (fmt_day(start, 0, 0, 0, 0), fmt_day(end, 23, 59, 59, 999))
            })
            .collect();

        let (r1, r2, r3, r4) = tokio::try_join!(
            self.fetch_demand_period(&periods[0].0, &periods[0].1, 1),
            self.fetch_demand_period(&periods[1].0, &periods[1].1, 2),
            self.fetch_demand_period(&periods[2].0, &periods[2].1, 3),
            self.fetch_demand_period(&periods[3].0, &periods[3].1, 4),
        )?;

        let nodes = [r1, r2, r3, r4]
            .into_iter()
            .flat_map(|(period, items)| items.into_iter().map(move |item| (period, item)))
            .enumerate()
            .map(|(i, (period, item))| item.into_demand_node(i + 1, period))
            .collect();

        Ok(nodes)
    }
}

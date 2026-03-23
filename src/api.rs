use chrono::{Duration, TimeZone, Utc};
use reqwest::{
    header::{self, HeaderMap, HeaderValue},
    Client,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use thiserror::Error;
use zeroize::Zeroize;

use crate::node::{CarKind, DemandNode, SupplyNode};

/// Формат дат в параметрах запроса к API.
// const DATE_FMT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";
const DATE_FMT: &str = "%Y-%m-%d";

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

        // Отфильтровываем узлы с нулевой потребностью до присвоения ID,
        // чтобы не раздувать размерность оптимизационной задачи.
        let nodes = [r1, r2, r3, r4]
            .into_iter()
            .flat_map(|(period, items)| items.into_iter().map(move |item| (period, item)))
            .filter(|(_, item)| {
                (item.planned_cars_to_load - item.provided_cars_to_load) > 0
            })
            .enumerate()
            .map(|(i, (period, item))| item.into_demand_node(i + 1, period))
            .collect();

        Ok(nodes)
    }

    /// Запрашивает узлы предложения порожних вагонов на текущую дату.
    ///
    /// API возвращает массив объектов (по одному на дорогу). Каждый объект
    /// содержит три группы: свободные именные, именные по факту, безномерные.
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
            return Err(ApiError::UnexpectedStatus {
                status: status.as_u16(),
                body,
            });
        }

        let railway_items = response.json::<Vec<SupplyApiItem>>().await?;

        let nodes = railway_items
            .into_iter()
            .flat_map(|item| {
                let numbered = item.numbered.into_iter().map(|c| {
                    let kind = if c.opz_railway_id.is_some() {
                        CarKind::Assigned
                    } else {
                        CarKind::Free
                    };
                    (kind, CarSource::Numbered(c))
                });
                let no_number = item.no_number.into_iter().map(|c| {
                    (CarKind::NoNumber, CarSource::NoNumber(c))
                });
                numbered.chain(no_number)
            })
            .enumerate()
            .map(|(i, (kind, source))| source.into_supply_node(i + 1, kind))
            .collect();

        Ok(nodes)
    }
}

// ---------------------------------------------------------------------------
// Supply API — внутренние структуры десериализации
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
    #[serde(rename = "StationFrom",      default)] station_from:      Option<String>,
    #[serde(rename = "StationFromCode",  default)] station_from_code: Option<String>,
    #[serde(rename = "RailWayFromShort", default)] railway_from_short: Option<String>,
    #[serde(rename = "RailWayFromCode",  default)] railway_from_code:  Option<i32>,
    #[serde(rename = "RailWayPartFrom",  default)] railway_part_from:  Option<String>,
    // Станция и дорога назначения
    #[serde(rename = "StationTo",      default)] station_to:      Option<String>,
    #[serde(rename = "StationToCode",  default)] station_to_code: Option<String>,
    #[serde(rename = "RailWayToShort", default)] railway_to_short: Option<String>,
    #[serde(rename = "RailWayToCode",  default)] railway_to_code:  Option<i32>,
    #[serde(rename = "RailWayPartTo",  default)] railway_part_to:  Option<String>,
    // OPZ-назначение: null = свободен (Free), не null = идёт по факту (Assigned)
    #[serde(rename = "OPZRailWayId")] opz_railway_id: Option<i64>,
    // Характеристики
    #[serde(rename = "CarCapacity",   default)] capacity:            f64,
    #[serde(rename = "CarBodyVolume", default)] volume:              f64,
    #[serde(rename = "CarModel",      default)] car_model:           Option<String>,
    // Груз
    #[serde(rename = "GRPOName",      default)] grpo_name:           Option<String>,
    #[serde(rename = "FrETSNGCode",   default)] etsng:               Option<String>,
    #[serde(rename = "FrETSNGName",   default)] etsng_name:          Option<String>,
    #[serde(rename = "PrevFrETSNGCode",default)] prev_etsng:         Option<String>,
    #[serde(rename = "PrevFrETSNGName",default)] prev_etsng_name:    Option<String>,
    // Ремонт
    #[serde(rename = "CarNextRepairDays",    default)] days_to_repair: Option<f64>,
    #[serde(rename = "CarNextRepairTypeName",default)] repair_type:    Option<String>,
    // Комментарии
    #[serde(rename = "CarCommentODO",  default)] comment_odo:   Option<String>,
    #[serde(rename = "CarCommentODO2", default)] comment_odo2:  Option<String>,
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
    #[serde(rename = "NextClaim",  default)] next_claim: Option<String>,
    #[serde(rename = "IdleTime",   default)] idle_time:  Option<f64>,
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
}

/// Безномерной вагон из `opzNoNumberModelCollection`.
#[derive(Deserialize, Debug)]
struct NoNumberItem {
    #[serde(rename = "StationToCode", default)] station_to_code:  Option<String>,
    #[serde(rename = "StationTo",     default)] station_to:       Option<String>,
    #[serde(rename = "RailWayToShort",default)] railway_to_short: Option<String>,
    #[serde(rename = "RailWayToCode", default)] railway_to_code:  Option<i32>,
    #[serde(rename = "RailWayPartTo", default)] railway_part_to:  Option<String>,
    #[serde(rename = "CarCount",      default)] car_count:        i32,
}

/// Вспомогательный enum для единой точки конвертации в [`SupplyNode`].
enum CarSource {
    Numbered(NumberedCarItem),
    NoNumber(NoNumberItem),
}

impl CarSource {
    fn into_supply_node(self, id: usize, kind: CarKind) -> SupplyNode {
        match self {
            CarSource::Numbered(c) => {
                let car_type     = c.car_type();
                let opz_comments = c.opz_comments();
                SupplyNode {
                    s_id: id,
                    kind,
                    car_number: Some(c.car_number),
                    car_count: 1,
                    station_from:      c.station_from,
                    station_from_code: c.station_from_code,
                    railway_from:      c.railway_from_short,
                    railway_from_code: c.railway_from_code,
                    railway_part_from: c.railway_part_from,
                    station_to:      c.station_to.unwrap_or_default(),
                    station_to_code: c.station_to_code.unwrap_or_default(),
                    railway_to:      c.railway_to_short.unwrap_or_default(),
                    railway_to_code: c.railway_to_code,
                    railway_part_to: c.railway_part_to,
                    capacity:         c.capacity,
                    volume:           c.volume,
                    car_type,
                    car_model:        c.car_model,
                    status:           c.grpo_name,
                    etsng:            c.etsng,
                    etsng_name:       c.etsng_name,
                    prev_etsng:       c.prev_etsng,
                    prev_etsng_name:  c.prev_etsng_name,
                    days_to_repair:   c.days_to_repair,
                    repair_type:      c.repair_type,
                    comment_odo:      c.comment_odo,
                    comment_odo2:     c.comment_odo2,
                    opz_comments,
                    next_claim:       c.next_claim,
                    idle_time:        c.idle_time,
                }
            }
            CarSource::NoNumber(c) => SupplyNode {
                s_id: id,
                kind,
                car_number: None,
                car_count:  c.car_count,
                station_from:      None,
                station_from_code: None,
                railway_from:      None,
                railway_from_code: None,
                railway_part_from: None,
                station_to:      c.station_to.unwrap_or_default(),
                station_to_code: c.station_to_code.unwrap_or_default(),
                railway_to:      c.railway_to_short.unwrap_or_default(),
                railway_to_code: c.railway_to_code,
                railway_part_to: c.railway_part_to,
                capacity:         0.0,
                volume:           0.0,
                car_type:         None,
                car_model:        None,
                status:           None,
                etsng:            None,
                etsng_name:       None,
                prev_etsng:       None,
                prev_etsng_name:  None,
                days_to_repair:   None,
                repair_type:      None,
                comment_odo:      None,
                comment_odo2:     None,
                opz_comments: None,
                next_claim:       None,
                idle_time:        None,
            },
        }
    }
}

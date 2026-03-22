use reqwest::{
    header::{self, HeaderMap, HeaderValue},
    Client,
};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use zeroize::Zeroize;

use crate::node::DemandNode;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("HTTP-запрос завершился ошибкой: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Неверный или просроченный токен (401 Unauthorized)")]
    Unauthorized,

    #[error("Неожиданный статус ответа {status}: {body}")]
    UnexpectedStatus { status: u16, body: String },
}

//Enpoints
    // base_api_url = "https://isupv-api.rusagrotrans.ru:2555/isupv/IsupvApi/"
    // supply_url = "https://isupv-api.rusagrotrans.ru:2555/isupv/IsupvApi/GetSupplyDataTransmission?docDate="
    // demand_url = "https://isupv-api.rusagrotrans.ru:2555/isupv/IsupvApi/GetDemandDataTransmission"
    // tariffs_url = "https://isupv-api.rusagrotrans.ru:2555/isupv/IsupvApi/GetRailTariffRouteDataTransmission"
    // output_url = "https://isupv-api.rusagrotrans.ru:2555/isupv/IsupvApi/DestinationRegistryTransmission"

//Params to demand endpoint GET request
// params = {
//         "LoadDateStart": "2026-03-15T15:11:51.390Z",
//         "LoadDateEnd": "2026-03-20T15:11:51.390Z",
//         "Page": 1,
//         "PageSize": 10000
//         }

//API Demand response Schema
// {
//   "meta": {
//     "timeZone": "string",
//     "generatedAt": "2026-03-22T18:49:17.434Z",
//     "storage": {
//       "datasetVersion": "2026-03-22T18:49:17.434Z",
//       "coverage": {
//         "from": "2026-03-22T18:49:17.434Z",
//         "to": "2026-03-22T18:49:17.434Z"
//       }
//     },
//     "freshness": {
//       "isFullyCoveredForRequest": true,
//       "warnings": [
//         "string"
//       ]
//     },
//     "page": 0,
//     "pageSize": 0,
//     "total": 0,
//     "pages": 0
//   },
//   "data": [
//     {
//       "LoadDateStart": "2026-03-22T18:49:17.434Z",
//       "LoadDateEnd": "2026-03-22T18:49:17.434Z",
//       "RailWayPartFrom": "string",
//       "RailWayShortFrom": "string",
//       "RailWayFromCode": "string",
//       "StationFrom": "string",
//       "StationFromCode": "string",
//       "RailWayPartTo": "string",
//       "RailWayShortTo": "string",
//       "RailWayToCode": "string",
//       "StationTo": "string",
//       "StationToCode": "string",
//       "Sender": "string",
//       "SenderOKPO": "string",
//       "SenderTGNL": "string",
//       "Client": [
//         "string"
//       ],
//       "CustomerOKPO": [
//         "string"
//       ],
//       "Recip": [
//         "string"
//       ],
//       "LoaderToOKPO": [
//         "string"
//       ],
//       "NameGNG": "string",
//       "FrETSNGCode": "string",
//       "DocumentNumber": [
//         "string"
//       ],
//       "DocumentDate": [
//         "2026-03-22T18:49:17.434Z"
//       ],
//       "GU12Number": [
//         "string"
//       ],
//       "LoadTypeName": "string",
//       "PlannedCarsToLoad": 0,
//       "PlannedWeightToLoad": 0,
//       "ProvidedCarsToLoad": 0,
//       "CarsOnStation": 0
//     }
//   ]
// }

/// Эндпойнты АПИ.
pub struct ApiEndpoints {
    Demand,
    Supply,
    Tariffs,
    Output
}

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
    ///
    /// # Errors
    /// Возвращает ошибку, если значение токена содержит недопустимые символы.
    pub fn new(base_url: impl Into<String>, token: &SecretString) -> Result<Self, ApiError> {
        let mut bearer = format!("Bearer {}", token.expose_secret());

        let mut auth_value = HeaderValue::from_str(&bearer).map_err(|_| {
            ApiError::UnexpectedStatus {
                status: 0,
                body: "Токен содержит недопустимые символы".to_string(),
            }
        })?;

        // Затираем временную строку с токеном сразу после передачи в HeaderValue.
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

    /// Запрашивает список узлов спроса (заявки на погрузку).
    ///
    /// Ожидает от API ответ `200 OK` с телом `application/json` — массивом
    /// объектов, десериализуемых в [`DemandNode`].
    pub async fn fetch_demand_nodes(&self) -> Result<Vec<DemandNode>, ApiError> {
        let url = format!("{}/demand_nodes", self.base_url);

        let response = self.client.get(&url).send().await?;

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

        let nodes = response.json::<Vec<DemandNode>>().await?;
        Ok(nodes)
    }
}

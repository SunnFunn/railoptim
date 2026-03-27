use reqwest::{
    header::{self, HeaderMap, HeaderValue},
    Client,
};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use zeroize::Zeroize;

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
pub enum ApiEndpoint {
    Demand,
    Supply,
    Tariffs,
    Output,
}

impl ApiEndpoint {
    pub fn path(&self) -> &'static str {
        match self {
            Self::Demand  => "GetDemandDataTransmission",
            Self::Supply  => "GetSupplyDataTransmission",
            Self::Tariffs => "GetRailTariffRouteDataTransmission",
            Self::Output  => "DestinationRegistryTransmission",
        }
    }

    pub fn url(&self, base_url: &str) -> String {
        format!("{}/{}", base_url, self.path())
    }
}

// ---------------------------------------------------------------------------
// Клиент
// ---------------------------------------------------------------------------

/// HTTP-клиент для API оптимизации.
///
/// Токен передаётся как [`SecretString`] и раскрывается только на время
/// формирования заголовка; временная строка затирается через [`zeroize`].
pub struct ApiClient {
    pub(super) client: Client,
    pub(super) base_url: String,
}

impl ApiClient {
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
}

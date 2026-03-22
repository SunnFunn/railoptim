use secrecy::SecretString;
use thiserror::Error;
use zeroize::Zeroize;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Переменная окружения {var} не задана: {source}")]
    MissingVar {
        var: &'static str,
        #[source]
        source: std::env::VarError,
    },
}

/// Конфигурация сервиса, загружаемая из переменных окружения.
///
/// Переменные инжектируются Infisical-агентом в процесс при старте.
///
/// | Переменная      | Описание                                          |
/// |-----------------|---------------------------------------------------|
/// | `API_BASE_URL`  | Корневой адрес API, напр. `http://10.0.0.5:8080`  |
/// | `API_TOKEN`     | Bearer-токен для аутентификации                   |
///
/// `api_token` хранится как [`SecretString`]: не отображается в логах/отладке
/// и автоматически затирается из памяти при дропе через [`zeroize`].
#[derive(Debug)]
pub struct Config {
    pub api_base_url: String,
    pub api_token: SecretString,
}

impl Config {
    /// Читает конфигурацию из переменных окружения.
    ///
    /// Возвращает [`ConfigError::MissingVar`], если хотя бы одна переменная отсутствует.
    pub fn from_env() -> Result<Self, ConfigError> {
        let api_base_url = std::env::var("API_BASE_URL").map_err(|e| ConfigError::MissingVar {
            var: "API_BASE_URL",
            source: e,
        })?;

        let mut raw_token = std::env::var("API_TOKEN").map_err(|e| ConfigError::MissingVar {
            var: "API_TOKEN",
            source: e,
        })?;

        let api_token = SecretString::new(raw_token.clone().into());

        // Затираем временную копию из стека сразу после обёртки в SecretString.
        raw_token.zeroize();

        Ok(Self {
            api_base_url,
            api_token,
        })
    }
}

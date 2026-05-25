//! Конфигурация web-сервера (`railoptim-web`).

use std::net::SocketAddr;
use std::path::PathBuf;

use thiserror::Error;

use crate::data::DEFAULT_DB_PATH;

#[derive(Debug, Error)]
pub enum WebConfigError {
    #[error("Переменная окружения {var} не задана: {source}")]
    MissingVar {
        var: &'static str,
        #[source]
        source: std::env::VarError,
    },
    #[error("Некорректный WEB_BIND_ADDR={value:?}: {source}")]
    InvalidBindAddr {
        value: String,
        #[source]
        source: std::net::AddrParseError,
    },
}

#[derive(Debug, Clone)]
pub struct WebConfig {
    pub bind_addr: SocketAddr,
    pub stations_geo_db: PathBuf,
    pub optim_result_dir: PathBuf,
    pub optim_result_file: Option<PathBuf>,
    pub cors_origins: Vec<String>,
    pub static_dir: Option<PathBuf>,
    pub map_dir: PathBuf,
}

impl WebConfig {
    pub fn from_env() -> Result<Self, WebConfigError> {
        let bind_raw = std::env::var("WEB_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
        let bind_addr = bind_raw.parse().map_err(|source| WebConfigError::InvalidBindAddr {
            value: bind_raw,
            source,
        })?;

        let stations_geo_db = std::env::var("STATIONS_GEO_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_DB_PATH));

        let optim_result_dir =
            std::env::var("OPTIM_RESULT_DIR").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("tmp"));

        let optim_result_file = std::env::var("OPTIM_RESULT_FILE")
            .ok()
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty());

        let cors_origins = std::env::var("WEB_CORS_ORIGINS")
            .unwrap_or_else(|_| "*".into())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let static_dir = std::env::var("WEB_STATIC_DIR")
            .ok()
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty());

        let map_dir = std::env::var("WEB_MAP_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/map"));

        Ok(Self {
            bind_addr,
            stations_geo_db,
            optim_result_dir,
            optim_result_file,
            cors_origins,
            static_dir,
            map_dir,
        })
    }
}

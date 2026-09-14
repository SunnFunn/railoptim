//! Шаг 1: чтение HASH `telegrams_db` из `conv-redis` (конвенции РЖД).
//!
//! Пока только подключение, разбор JSON и снимок одной телеграммы на диск —
//! в солвер записи ещё не идут. Нет секрета или Redis недоступен → fail-open.
//!
//! Переменные окружения (не путать с `REDIS_SUPPLY_*` дислокации на порту 6380):
//!   `REDIS_CONV_HOST` (по умолчанию `127.0.0.1`)
//!   `REDIS_CONV_PORT` (по умолчанию `6379`)
//!   `REDIS_CONV_DB`   (по умолчанию `0`)
//!   `REDIS_CONV_PASS` — пароль `conv-redis` (Infisical, тот же `REDIS_CONV_PASS`)
//!   `REDIS_CONV_SAMPLE` — номер телеграммы для снимка (по умолчанию `4702`)

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::Local;
use redis::Commands;
use serde::{Deserialize, Serialize};

/// HASH в db 0 контейнера `conv-redis`.
pub const TELEGRAMS_HASH: &str = "telegrams_db";

/// Снимок для просмотра в репозитории (gitignore — боевой дамп не коммитим).
pub const DUMP_PATH: &str = "data/conventions_from_redis.json";

/// Номер телеграммы из плана (дорожный All без ЕСР), если есть в HASH.
pub const DEFAULT_SAMPLE_NUMBER: &str = "4702";

/// Статус, с которым `railconventions` положил запись в HASH.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", content = "expiration_date")]
pub enum ConventionStatus {
    Preliminary(String),
    Approved(String),
    Historical(String),
    WashingStation,
    ReserveStation,
    RepairStation,
    Other,
    Unknown,
}

/// JSON-значение поля HASH `telegrams_db` (как пишет `railconventions`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct TelegramData {
    pub id: i32,
    pub rzd_number: String,
    pub date_create: String,
    pub date_beg: String,
    pub date_end: String,
    pub text: String,
    pub cargo_class: String,
    pub cargo_name: String,
    pub departure_st: String,
    pub departure_st_code: String,
    pub destination_st: String,
    pub destination_st_code: String,
    pub railroad_junction: String,
    pub recipient_name: String,
    pub recipient_okpo: String,
    pub convention_info: ConventionStatus,
}

#[derive(Clone)]
pub struct ConventionsRedisSettings {
    pub host: String,
    pub port: u16,
    pub db: i64,
    password: String,
}

impl ConventionsRedisSettings {
    /// `None` — пароль не задан, к Redis не ходим.
    pub fn from_env() -> Option<Self> {
        let password = std::env::var("REDIS_CONV_PASS").ok().filter(|s| !s.is_empty())?;
        let host = std::env::var("REDIS_CONV_HOST")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let port = std::env::var("REDIS_CONV_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(6379);
        let db = std::env::var("REDIS_CONV_DB")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        Some(Self {
            host,
            port,
            db,
            password,
        })
    }

    fn redis_url(&self) -> String {
        format!(
            "redis://:{}@{}:{}/{}",
            percent_encode_password(&self.password),
            self.host,
            self.port,
            self.db
        )
    }
}

/// Итог пробного чтения HASH (для лога и файла).
#[derive(Debug, Clone, Serialize)]
pub struct ConventionsProbe {
    pub fetched_at: String,
    pub host: String,
    pub port: u16,
    pub db: i64,
    pub hash: String,
    pub hash_fields: usize,
    pub sample_rzd_number: String,
    pub sample: TelegramData,
    pub rzd_numbers: Vec<String>,
}

/// Fail-open обёртка для `main`: нет пароля / ошибка Redis — предупреждение, процесс идёт дальше.
pub fn probe_conventions_at_startup() {
    match dump_conventions_stub(None) {
        Ok(probe) => {
            println!(
                "Конвенции (шаг 1, conv-redis): подключено {}:{}/{} HASH `{}` — {} записей; снимок №{} → {}",
                probe.host,
                probe.port,
                probe.db,
                probe.hash,
                probe.hash_fields,
                probe.sample_rzd_number,
                DUMP_PATH,
            );
            println!(
                "  образец: class={} | груз={} | отпр.={} | назн.={} | получатель={} | {}…{}",
                probe.sample.cargo_class,
                probe.sample.cargo_name,
                probe.sample.departure_st,
                probe.sample.destination_st,
                probe.sample.recipient_name,
                probe.sample.date_beg,
                probe.sample.date_end,
            );
        }
        Err(e) => {
            eprintln!("  [!] конвенции conv-redis: {e} — правило 5 пока не применяется");
        }
    }
}

/// Читает HASH, выбирает тестовую телеграмму, пишет [`DUMP_PATH`].
pub fn dump_conventions_stub(dump_path: Option<&Path>) -> Result<ConventionsProbe> {
    let settings = ConventionsRedisSettings::from_env().context(
        "REDIS_CONV_PASS не задан (Infisical / окружение) — к conv-redis не подключаюсь",
    )?;
    let path = dump_path.unwrap_or(Path::new(DUMP_PATH));
    fetch_and_dump(&settings, path)
}

fn fetch_and_dump(settings: &ConventionsRedisSettings, dump_path: &Path) -> Result<ConventionsProbe> {
    let client = redis::Client::open(settings.redis_url()).context("URL Redis conv-redis")?;
    let mut con = client
        .get_connection()
        .with_context(|| format!("подключение к conv-redis {}:{}", settings.host, settings.port))?;

    let raw: HashMap<String, String> = con
        .hgetall(TELEGRAMS_HASH)
        .with_context(|| format!("HGETALL {TELEGRAMS_HASH}"))?;

    if raw.is_empty() {
        bail!("HASH `{TELEGRAMS_HASH}` пуст — парсер railconventions ещё не писал результаты");
    }

    let mut rzd_numbers: Vec<String> = raw.keys().cloned().collect();
    rzd_numbers.sort();

    let preferred = std::env::var("REDIS_CONV_SAMPLE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SAMPLE_NUMBER.to_string());

    let sample_key = if raw.contains_key(&preferred) {
        preferred
    } else {
        rzd_numbers[0].clone()
    };

    let sample_json = raw
        .get(&sample_key)
        .context("выбранный ключ исчез из HASH")?;
    let sample: TelegramData = serde_json::from_str(sample_json).with_context(|| {
        format!("разбор JSON телеграммы №{sample_key} из `{TELEGRAMS_HASH}`")
    })?;

    let probe = ConventionsProbe {
        fetched_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        host: settings.host.clone(),
        port: settings.port,
        db: settings.db,
        hash: TELEGRAMS_HASH.to_string(),
        hash_fields: raw.len(),
        sample_rzd_number: sample_key,
        sample,
        rzd_numbers,
    };

    if let Some(parent) = dump_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("каталог {}", parent.display()))?;
    }
    let pretty = serde_json::to_string_pretty(&probe).context("сериализация снимка")?;
    fs::write(dump_path, pretty).with_context(|| format!("запись {}", dump_path.display()))?;
    Ok(probe)
}

fn percent_encode_password(raw: &str) -> String {
    let mut out = String::new();
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_4702: &str = r#"{
        "id": 8,
        "rzd_number": "4702",
        "date_create": "2026-03-11",
        "date_beg": "2026-03-03",
        "date_end": "2026-03-04",
        "text": "ограничение погрузки всех грузов на 50%",
        "cargo_class": "All",
        "cargo_name": "все грузы",
        "departure_st": "Все станции Красноярской, Западно-Сибирской, Южно-Уральской и Свердловской железных дорог",
        "departure_st_code": "Все станции",
        "destination_st": "Все станции Октябрьской, Северной, Московской и Горьковской железных дорог",
        "destination_st_code": "Все станции",
        "railroad_junction": "None",
        "recipient_name": "все грузополучатели",
        "recipient_okpo": "All",
        "convention_info": {"type": "Other"}
    }"#;

    #[test]
    fn parses_telegram_4702_from_redis() {
        let t: TelegramData = serde_json::from_str(SAMPLE_4702).unwrap();
        assert_eq!(t.rzd_number, "4702");
        assert_eq!(t.cargo_class, "All");
        assert_eq!(t.departure_st_code, "Все станции");
        assert_eq!(t.recipient_okpo, "All");
        assert_eq!(t.convention_info, ConventionStatus::Other);
    }

    #[test]
    fn parses_preliminary_status_with_date() {
        let json = r#"{"type":"Preliminary","expiration_date":"2026-12-01"}"#;
        let s: ConventionStatus = serde_json::from_str(json).unwrap();
        assert_eq!(s, ConventionStatus::Preliminary("2026-12-01".into()));
    }

    #[test]
    fn repo_example_4702_matches_redis_shape() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data/conventions/telegram_4702.example.json");
        let text = std::fs::read_to_string(&path).unwrap();
        let t: TelegramData = serde_json::from_str(&text).unwrap();
        assert_eq!(t.rzd_number, "4702");
        assert_eq!(t.convention_info, ConventionStatus::Other);
        assert!(t.text.contains("Ферма"));
    }

    #[test]
    fn percent_encodes_specials_in_password() {
        assert_eq!(percent_encode_password("conv"), "conv");
        assert_eq!(percent_encode_password("a/b@c"), "a%2Fb%40c");
    }
}

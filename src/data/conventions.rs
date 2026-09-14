//! Правило 5: конвенциональные телеграммы РЖД из HASH `telegrams_db` (`conv-redis`).
//!
//! Шаг 1 — подключение. Шаг 2 — разбор JSON, в память и снимок только действующие
//! (даты пересекают горизонт планирования). В солвер (`classify_pair`) записи пока не идут.
//!
//! Переменные окружения (не путать с `REDIS_SUPPLY_*` дислокации на порту 6380):
//!   `REDIS_CONV_HOST` (по умолчанию `127.0.0.1`)
//!   `REDIS_CONV_PORT` (по умолчанию `6379`)
//!   `REDIS_CONV_DB`   (по умолчанию `0`)
//!   `REDIS_CONV_PASS` — пароль `conv-redis`

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{Duration, Local, NaiveDate};
use redis::Commands;
use serde::{Deserialize, Serialize};

use super::demand::DEMAND_PERIODS;
use super::esr::normalize_esr6;
use super::gu12::{normalize_okpo, normalize_party_name};

/// HASH в db 0 контейнера `conv-redis`.
pub const TELEGRAMS_HASH: &str = "telegrams_db";

/// Снимок только действующих телеграмм (gitignore — боевой дамп не коммитим).
pub const DUMP_PATH: &str = "data/conventions_from_redis.json";

/// Маппинг «Октябрьская ЖД» → `ОКТ`.
pub const RAILWAY_MAP_PATH: &str = "data/map/supermap_rw_name_to_rw.csv";

/// Короткий код Казахстанской ЖД: дорожный запрет «все станции КЗХ» не применяем.
const KZH_RAILWAY: &str = "КЗХ";

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

impl ConventionStatus {
    fn is_service_station(&self) -> bool {
        matches!(
            self,
            Self::WashingStation | Self::ReserveStation | Self::RepairStation
        )
    }
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

/// Класс груза, который правило 5 учитывает в v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ConventionCargoClass {
    Empty,
    All,
    Grain,
}

impl ConventionCargoClass {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "Empty" => Some(Self::Empty),
            "All" => Some(Self::All),
            "Grain" => Some(Self::Grain),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "Empty",
            Self::All => "All",
            Self::Grain => "Grain",
        }
    }
}

/// Разобранная действующая конвенция (ещё без индекса солвера).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ParsedConvention {
    pub rzd_number: String,
    pub cargo_class: ConventionCargoClass,
    pub cargo_name: String,
    pub date_beg: String,
    pub date_end: String,
    pub dest_esr: Vec<String>,
    pub dest_names: Vec<String>,
    pub dest_railways: Vec<String>,
    pub dest_all_stations: bool,
    pub dep_esr: Vec<String>,
    pub dep_names: Vec<String>,
    pub dep_railways: Vec<String>,
    pub dep_all_stations: bool,
    pub junction: Option<String>,
    pub all_parties: bool,
    pub recipient_okpo: Vec<String>,
    pub recipient_names: Vec<String>,
    pub unknown_road_fragments: Vec<String>,
    pub convention_info: ConventionStatus,
    /// С назначения сняли КЗХ (для отброса «все станции только КЗХ»).
    #[serde(skip)]
    kzh_stripped_dest: bool,
    /// С отправления сняли КЗХ.
    #[serde(skip)]
    kzh_stripped_dep: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ConventionsLoadStats {
    pub hash_fields: usize,
    pub parsed_ok: usize,
    pub bad_json: usize,
    pub expired: usize,
    pub not_yet: usize,
    pub empty_dates: usize,
    pub skipped_class: usize,
    pub skipped_kzh: usize,
    pub active: usize,
    pub active_empty: usize,
    pub active_all: usize,
    pub active_grain: usize,
    pub active_service: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConventionsLoad {
    pub stats: ConventionsLoadStats,
    pub active: Vec<ParsedConvention>,
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

/// Снимок: размер HASH в Redis и список действующих (для файла и лога).
#[derive(Debug, Clone, Serialize)]
pub struct ConventionsProbe {
    pub fetched_at: String,
    pub host: String,
    pub port: u16,
    pub db: i64,
    pub hash: String,
    pub hash_fields: usize,
    pub load: ConventionsLoad,
}

/// Горизонт спроса: последний день периода 4 ([`DEMAND_PERIODS`]).
pub fn planning_horizon_end(today: NaiveDate) -> NaiveDate {
    let last = DEMAND_PERIODS.last().map(|(_, e)| *e).unwrap_or(14);
    today + Duration::days(last)
}

/// Fail-open для `main`. `enabled == false` — даже к Redis не ходим.
pub fn load_conventions_at_startup(enabled: bool) {
    if !enabled {
        println!("Конвенции (правило 5): выкл. (ConventionCheckEnabled=false)");
        return;
    }
    match dump_conventions_stub(None) {
        Ok(probe) => {
            let st = &probe.load.stats;
            println!(
                "Конвенции (правило 5, conv-redis): {}:{}/{} HASH `{}` — {} записей в Redis, в сервис взято {} действующих → {}",
                probe.host,
                probe.port,
                probe.db,
                probe.hash,
                probe.hash_fields,
                st.active,
                DUMP_PATH,
            );
            println!(
                "  в HASH не взяты (истёкшие/вне горизонта/без дат, в память не кладём): истекло {}, ещё не началось {}, без дат {}; битый JSON {}",
                st.expired, st.not_yet, st.empty_dates, st.bad_json,
            );
            println!(
                "  отброшено из действующих по дате: класс Others/ошибка {}, только КЗХ {}",
                st.skipped_class, st.skipped_kzh,
            );
            println!(
                "  действующих: {} (Empty {}, All {}, Grain {}; из них промывка/ремонт/отстой {})",
                st.active, st.active_empty, st.active_all, st.active_grain, st.active_service,
            );
            for item in probe.load.active.iter().take(15) {
                println!("    · {}", format_active_line(item));
            }
            if probe.load.active.len() > 15 {
                println!("    · ...ещё {} действующих", probe.load.active.len() - 15);
            }
        }
        Err(e) => {
            eprintln!("  [!] конвенции conv-redis: {e} — правило 5 не применяется");
        }
    }
}

/// Читает HASH, оставляет только действующие, пишет [`DUMP_PATH`].
pub fn dump_conventions_stub(dump_path: Option<&Path>) -> Result<ConventionsProbe> {
    let settings = ConventionsRedisSettings::from_env().context(
        "REDIS_CONV_PASS не задан (Infisical / окружение) — к conv-redis не подключаюсь",
    )?;
    let path = dump_path.unwrap_or(Path::new(DUMP_PATH));
    fetch_parse_and_dump(&settings, path)
}

fn fetch_parse_and_dump(
    settings: &ConventionsRedisSettings,
    dump_path: &Path,
) -> Result<ConventionsProbe> {
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

    let catalog = RailwayCatalog::load_default().unwrap_or_else(|e| {
        eprintln!(
            "  [!] справочник дорог ({RAILWAY_MAP_PATH}): {e} — дорожные «все станции» не разберутся"
        );
        RailwayCatalog::default()
    });
    let today = Local::now().date_naive();
    let load = parse_hash(&raw, today, &catalog);
    drop(raw);

    let probe = ConventionsProbe {
        fetched_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        host: settings.host.clone(),
        port: settings.port,
        db: settings.db,
        hash: TELEGRAMS_HASH.to_string(),
        hash_fields: load.stats.hash_fields,
        load,
    };

    if let Some(parent) = dump_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("каталог {}", parent.display()))?;
    }
    let pretty = serde_json::to_string_pretty(&probe).context("сериализация снимка")?;
    fs::write(dump_path, pretty).with_context(|| format!("запись {}", dump_path.display()))?;
    Ok(probe)
}

/// Разбор уже прочитанного HASH (для тестов без Redis).
/// В `active` только записи, действующие на горизонт `today … today+14`.
pub fn parse_hash(
    raw: &HashMap<String, String>,
    today: NaiveDate,
    catalog: &RailwayCatalog,
) -> ConventionsLoad {
    let horizon = planning_horizon_end(today);
    let mut stats = ConventionsLoadStats {
        hash_fields: raw.len(),
        ..Default::default()
    };
    let mut active = Vec::new();

    for (key, json) in raw {
        let tel: TelegramData = match serde_json::from_str(json) {
            Ok(t) => t,
            Err(_) => {
                stats.bad_json += 1;
                continue;
            }
        };
        stats.parsed_ok += 1;

        let beg = parse_iso_date(&tel.date_beg);
        let end = parse_iso_date(&tel.date_end);
        match (beg, end) {
            (None, _) | (_, None) => {
                stats.empty_dates += 1;
                continue;
            }
            (Some(_), Some(e)) if e < today => {
                stats.expired += 1;
                continue;
            }
            (Some(b), Some(_)) if b > horizon => {
                stats.not_yet += 1;
                continue;
            }
            (Some(_), Some(_)) => {}
        }

        let Some(class) = resolve_cargo_class(&tel) else {
            stats.skipped_class += 1;
            continue;
        };

        let mut parsed = parse_telegram_fields(&tel, class, catalog);
        if parsed.rzd_number.is_empty() {
            parsed.rzd_number = key.clone();
        }
        if is_kzh_only_road_ban(&parsed) {
            stats.skipped_kzh += 1;
            continue;
        }
        match parsed.cargo_class {
            ConventionCargoClass::Empty => stats.active_empty += 1,
            ConventionCargoClass::All => stats.active_all += 1,
            ConventionCargoClass::Grain => stats.active_grain += 1,
        }
        if parsed.convention_info.is_service_station() {
            stats.active_service += 1;
        }
        stats.active += 1;
        active.push(parsed);
    }

    active.sort_by(|a, b| a.rzd_number.cmp(&b.rzd_number));
    ConventionsLoad { stats, active }
}

fn resolve_cargo_class(tel: &TelegramData) -> Option<ConventionCargoClass> {
    if let Some(class) = ConventionCargoClass::parse(&tel.cargo_class) {
        return Some(class);
    }
    // Промывка/ремонт/отстой в HASH часто с классом Others — это назначения Empty.
    if tel.convention_info.is_service_station() {
        Some(ConventionCargoClass::Empty)
    } else {
        None
    }
}

/// Дорожный запрет «все станции» только КЗХ (остальные инотерритории оставляем).
fn is_kzh_only_road_ban(p: &ParsedConvention) -> bool {
    let dest_empty = p.dest_esr.is_empty() && p.dest_railways.is_empty();
    let dep_empty = p.dep_esr.is_empty() && p.dep_railways.is_empty();
    if p.dest_all_stations && dest_empty && p.kzh_stripped_dest {
        return true;
    }
    dest_empty && dep_empty && p.dep_all_stations && p.kzh_stripped_dep
}

fn parse_telegram_fields(
    tel: &TelegramData,
    class: ConventionCargoClass,
    catalog: &RailwayCatalog,
) -> ParsedConvention {
    let dest_esr = parse_esr_list(&tel.destination_st_code);
    let dep_esr = parse_esr_list(&tel.departure_st_code);
    let dest_names = parse_station_names(&tel.destination_st);
    let dep_names = parse_station_names(&tel.departure_st);

    let (mut dest_roads, mut dest_unknown) = catalog.parse_railways(&tel.destination_st);
    let (mut dep_roads, mut dep_unknown) = catalog.parse_railways(&tel.departure_st);
    let kzh_stripped_dest = strip_kzh(&mut dest_roads);
    let kzh_stripped_dep = strip_kzh(&mut dep_roads);

    let dest_all_stations = dest_esr.is_empty() && looks_like_all_stations(&tel.destination_st_code, &tel.destination_st);
    let dep_all_stations = dep_esr.is_empty() && looks_like_all_stations(&tel.departure_st_code, &tel.departure_st);

    dest_unknown.append(&mut dep_unknown);

    let (all_parties, recipient_okpo, recipient_names) =
        parse_parties(&tel.recipient_okpo, &tel.recipient_name);

    let junction = {
        let j = tel.railroad_junction.trim();
        if j.is_empty() || j.eq_ignore_ascii_case("none") {
            None
        } else {
            Some(j.to_string())
        }
    };

    ParsedConvention {
        rzd_number: tel.rzd_number.clone(),
        cargo_class: class,
        cargo_name: tel.cargo_name.clone(),
        date_beg: tel.date_beg.clone(),
        date_end: tel.date_end.clone(),
        dest_esr,
        dest_names,
        dest_railways: dest_roads,
        dest_all_stations,
        dep_esr,
        dep_names,
        dep_railways: dep_roads,
        dep_all_stations,
        junction,
        all_parties,
        recipient_okpo,
        recipient_names,
        unknown_road_fragments: dest_unknown,
        convention_info: tel.convention_info.clone(),
        kzh_stripped_dest,
        kzh_stripped_dep,
    }
}

fn strip_kzh(codes: &mut Vec<String>) -> bool {
    let before = codes.len();
    codes.retain(|c| c != KZH_RAILWAY);
    before != codes.len()
}

fn parse_iso_date(raw: &str) -> Option<NaiveDate> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn parse_esr_list(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in raw.split([',', ';']) {
        let p = part.trim();
        let digits: String = p.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() == 6 {
            let n = normalize_esr6(&digits);
            if n.len() == 6 && !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

fn parse_station_names(raw: &str) -> Vec<String> {
    split_list(raw)
        .into_iter()
        .filter(|s| !looks_like_all_stations(s, s) && !s.eq_ignore_ascii_case("none"))
        .collect()
}

fn looks_like_all_stations(code: &str, name: &str) -> bool {
    let blob = format!("{} {}", norm_ru(code), norm_ru(name));
    blob.contains("все станции") || blob.contains("всех станц") || blob.contains("всех железнодорожн")
}

fn parse_parties(okpo_raw: &str, name_raw: &str) -> (bool, Vec<String>, Vec<String>) {
    let okpo_trim = okpo_raw.trim();
    let name_trim = name_raw.trim();
    let all_token = okpo_trim.eq_ignore_ascii_case("all")
        || okpo_trim.eq_ignore_ascii_case("not specified")
        || norm_ru(name_trim).contains("все грузополучател")
        || (okpo_trim.is_empty() && name_trim.is_empty());

    let mut okpos = Vec::new();
    if !all_token {
        for part in split_list(okpo_raw) {
            if part.eq_ignore_ascii_case("all") || part.eq_ignore_ascii_case("not specified") {
                continue;
            }
            let n = normalize_okpo(&part);
            if !n.is_empty() && !okpos.contains(&n) {
                okpos.push(n);
            }
        }
    }

    let mut names = Vec::new();
    if !all_token {
        for part in split_list(name_raw) {
            if norm_ru(&part).contains("все грузополучател") || norm_ru(&part) == "и другие" {
                continue;
            }
            let n = normalize_party_name(&part);
            if !n.is_empty() && !names.contains(&n) {
                names.push(n);
            }
        }
    }

    let all_parties = all_token || (okpos.is_empty() && names.is_empty());
    (all_parties, okpos, names)
}

fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .flat_map(|p| p.split(" и "))
        .map(|s| s.trim().trim_matches('"').trim_matches('«').trim_matches('»'))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn format_active_line(p: &ParsedConvention) -> String {
    let dest = if !p.dest_esr.is_empty() {
        format!("назн.ЕСР {}", p.dest_esr.join(","))
    } else if !p.dest_railways.is_empty() {
        format!("назн.дороги {}", p.dest_railways.join(","))
    } else if p.dest_all_stations {
        "назн.все станции".to_string()
    } else {
        p.dest_names.join(", ")
    };
    let dep = if !p.dep_railways.is_empty() {
        format!("отпр.{}", p.dep_railways.join(","))
    } else if !p.dep_esr.is_empty() {
        format!("отпр.ЕСР {}", p.dep_esr.join(","))
    } else {
        String::new()
    };
    let party = if p.all_parties {
        "все получатели"
    } else {
        "по ОКПО/имени"
    };
    let kind = match p.convention_info {
        ConventionStatus::WashingStation => "промывка",
        ConventionStatus::ReserveStation => "отстой",
        ConventionStatus::RepairStation => "ремонт",
        _ => p.cargo_class.as_str(),
    };
    if dep.is_empty() {
        format!(
            "№{} {} | {} | {} | {}…{} | {}",
            p.rzd_number,
            kind,
            dest,
            party,
            p.date_beg,
            p.date_end,
            p.cargo_name,
        )
    } else {
        format!(
            "№{} {} | {} → {} | {} | {}…{} | {}",
            p.rzd_number,
            kind,
            dep,
            dest,
            party,
            p.date_beg,
            p.date_end,
            p.cargo_name,
        )
    }
}

fn norm_ru(s: &str) -> String {
    s.trim().to_lowercase().replace('ё', "е")
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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

// --- Справочник дорог --------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct RailwayCatalog {
    /// Основа названия (после снятия падежа) → короткий код.
    stems: Vec<(String, String)>,
}

impl RailwayCatalog {
    pub fn load_default() -> Result<Self> {
        let path = railway_map_file();
        Self::load_csv(&path)
    }

    pub fn load_csv(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("чтение {}", path.display()))?;
        let mut stems = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || (i == 0 && line.starts_with("supermap")) {
                continue;
            }
            let Some((name, code)) = line.split_once(',') else {
                continue;
            };
            let code = code.trim().to_uppercase();
            if code.is_empty() {
                continue;
            }
            let stem = adjective_stem(&strip_railway_boilerplate(name));
            if !stem.is_empty() {
                stems.push((stem, code.clone()));
            }
            stems.push((norm_ru(&code), code));
        }
        stems.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.1.cmp(&b.1)));
        stems.dedup();
        Ok(Self { stems })
    }

    /// Короткие коды дорог, упомянутых в строке телеграммы (включая инотерритории кроме КЗХ на этапе фильтра).
    pub fn parse_railways(&self, raw: &str) -> (Vec<String>, Vec<String>) {
        let stripped = strip_railway_boilerplate(raw);
        if stripped.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut codes = Vec::new();
        let mut unknown = Vec::new();
        for frag in split_list(&stripped) {
            let frag = strip_railway_boilerplate(&frag);
            if frag.is_empty() {
                continue;
            }
            if let Some(code) = self.match_fragment(&frag) {
                if !codes.contains(&code) {
                    codes.push(code);
                }
            } else if looks_like_all_stations(&frag, &frag) {
                continue;
            } else {
                unknown.push(frag);
            }
        }
        (codes, unknown)
    }

    fn match_fragment(&self, frag: &str) -> Option<String> {
        let f = collapse_ws(&norm_ru(frag));
        if f.is_empty() {
            return None;
        }
        let f_stem = adjective_stem(&f);
        for (stem, code) in &self.stems {
            if f_stem == *stem || f == *stem {
                return Some(code.clone());
            }
        }
        None
    }
}

fn railway_map_file() -> PathBuf {
    let cwd = PathBuf::from(RAILWAY_MAP_PATH);
    if cwd.exists() {
        return cwd;
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join(RAILWAY_MAP_PATH)
}

fn strip_railway_boilerplate(raw: &str) -> String {
    let mut t = collapse_ws(&norm_ru(raw));
    const PATS: &[&str] = &[
        "назначением на",
        "со всех железнодорожных станций",
        "со всех станций",
        "на все железнодорожные станции",
        "на все станции",
        "все железнодорожные станции",
        "все станции",
        "железнодорожных станций",
        "железных дорог",
        "железной дороги",
        "железная дорога",
        " жд",
    ];
    for pat in PATS {
        t = t.replace(pat, " ");
    }
    collapse_ws(&t.replace(['(', ')'], " "))
}

fn adjective_stem(s: &str) -> String {
    let t = collapse_ws(&norm_ru(s));
    const SUFFIXES: &[&str] = &[
        "ских", "ский", "ская", "ской", "скую", "ское", "ские", "ая", "ой", "ую", "ое", "ые", "ый",
    ];
    for suf in SUFFIXES {
        if let Some(rest) = t.strip_suffix(suf).filter(|rest| rest.chars().count() >= 4) {
            return rest.to_string();
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    const LLM_FAILED_MARKER: &str = "LLM_ERROR";

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

    fn catalog() -> RailwayCatalog {
        RailwayCatalog::load_csv(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(RAILWAY_MAP_PATH),
        )
        .unwrap()
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 14).unwrap()
    }

    fn load_of(raw: &HashMap<String, String>) -> ConventionsLoad {
        parse_hash(raw, today(), &catalog())
    }

    fn tel_json(class: &str, beg: &str, end: &str, dest: &str, dest_code: &str, okpo: &str) -> String {
        format!(
            r#"{{
                "id": 1,
                "rzd_number": "1",
                "date_create": "2026-09-01",
                "date_beg": "{beg}",
                "date_end": "{end}",
                "text": "t",
                "cargo_class": "{class}",
                "cargo_name": "x",
                "departure_st": "Все станции",
                "departure_st_code": "Все станции",
                "destination_st": "{dest}",
                "destination_st_code": "{dest_code}",
                "railroad_junction": "None",
                "recipient_name": "все грузополучатели",
                "recipient_okpo": "{okpo}",
                "convention_info": {{"type": "Other"}}
            }}"#
        )
    }

    #[test]
    fn parses_telegram_4702_from_redis() {
        let t: TelegramData = serde_json::from_str(SAMPLE_4702).unwrap();
        assert_eq!(t.rzd_number, "4702");
        assert_eq!(t.cargo_class, "All");
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
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data/conventions/telegram_4702.example.json");
        let text = fs::read_to_string(&path).unwrap();
        let t: TelegramData = serde_json::from_str(&text).unwrap();
        assert_eq!(t.rzd_number, "4702");
        assert!(t.text.contains("Ферма"));
    }

    #[test]
    fn percent_encodes_specials_in_password() {
        assert_eq!(percent_encode_password("conv"), "conv");
        assert_eq!(percent_encode_password("a/b@c"), "a%2Fb%40c");
    }

    #[test]
    fn railways_4702_stems_to_short_codes() {
        let cat = catalog();
        let t: TelegramData = serde_json::from_str(SAMPLE_4702).unwrap();
        let parsed = parse_telegram_fields(&t, ConventionCargoClass::All, &cat);
        let mut dep = parsed.dep_railways.clone();
        dep.sort();
        assert_eq!(dep, vec!["ЗСБ", "КРС", "СВР", "ЮУР"]);
        let mut dest = parsed.dest_railways.clone();
        dest.sort();
        assert_eq!(dest, vec!["ГОР", "МСК", "ОКТ", "СЕВ"]);
        assert!(parsed.dest_all_stations);
        assert!(parsed.dep_all_stations);
        assert!(parsed.all_parties);
        assert!(parsed.dest_esr.is_empty());
    }

    #[test]
    fn expired_4702_is_dropped_on_sep_2026() {
        let mut raw = HashMap::new();
        raw.insert("4702".into(), SAMPLE_4702.to_string());
        let load = load_of(&raw);
        assert_eq!(load.stats.expired, 1);
        assert_eq!(load.stats.active, 0);
    }

    #[test]
    fn expired_others_counts_as_expired_not_class() {
        let mut raw = HashMap::new();
        raw.insert(
            "o".into(),
            tel_json("Others", "2026-01-01", "2026-03-04", "Все станции СКВ", "Все станции", "All"),
        );
        let load = load_of(&raw);
        assert_eq!(load.stats.expired, 1);
        assert_eq!(load.stats.skipped_class, 0);
        assert_eq!(load.stats.active, 0);
    }

    #[test]
    fn open_ended_date_is_active() {
        let mut raw = HashMap::new();
        raw.insert(
            "1".into(),
            tel_json("Grain", "2026-01-01", "3000-01-01", "Новороссийск (эксп.)", "514003", "All"),
        );
        let load = load_of(&raw);
        assert_eq!(load.stats.active, 1);
        assert_eq!(load.active[0].dest_esr, vec!["514003"]);
        assert_eq!(load.active[0].cargo_class, ConventionCargoClass::Grain);
        assert!(!load.active[0].dest_all_stations);
    }

    #[test]
    fn empty_dates_and_others_and_llm_error_skipped() {
        let mut raw = HashMap::new();
        raw.insert("a".into(), tel_json("All", "", "2026-12-01", "Все станции СКВ", "Все станции", "All"));
        raw.insert("b".into(), tel_json("Others", "2026-01-01", "3000-01-01", "Все станции СКВ", "Все станции", "All"));
        raw.insert("c".into(), tel_json(LLM_FAILED_MARKER, "2026-01-01", "3000-01-01", "Все станции СКВ", "Все станции", "All"));
        let load = load_of(&raw);
        assert_eq!(load.stats.empty_dates, 1);
        assert_eq!(load.stats.skipped_class, 2);
        assert_eq!(load.stats.active, 0);
    }

    #[test]
    fn kazakhstan_all_stations_ignored() {
        let mut raw = HashMap::new();
        raw.insert(
            "k".into(),
            tel_json(
                "All",
                "2026-01-01",
                "3000-01-01",
                "Все станции Казахстанских железных дорог",
                "Все станции",
                "All",
            ),
        );
        let load = load_of(&raw);
        assert_eq!(load.stats.skipped_kzh, 1);
        assert_eq!(load.stats.active, 0);
    }

    #[test]
    fn belarus_all_stations_kept() {
        let mut raw = HashMap::new();
        raw.insert(
            "b".into(),
            tel_json(
                "All",
                "2026-01-01",
                "3000-01-01",
                "Все станции Белорусской железной дороги",
                "Все станции",
                "All",
            ),
        );
        let load = load_of(&raw);
        assert_eq!(load.stats.active, 1);
        assert_eq!(load.active[0].dest_railways, vec!["БЕЛ"]);
        assert_eq!(load.stats.skipped_kzh, 0);
    }

    #[test]
    fn kzh_stripped_from_mixed_dest_keeps_okt() {
        let mut raw = HashMap::new();
        raw.insert(
            "m".into(),
            tel_json(
                "All",
                "2026-01-01",
                "3000-01-01",
                "Все станции Октябрьской и Казахстанских железных дорог",
                "Все станции",
                "All",
            ),
        );
        let load = load_of(&raw);
        assert_eq!(load.stats.active, 1);
        assert_eq!(load.active[0].dest_railways, vec!["ОКТ"]);
    }

    #[test]
    fn skv_short_code_in_all_stations() {
        let cat = catalog();
        let (codes, _) = cat.parse_railways("Все станции СКВ");
        assert_eq!(codes, vec!["СКВ"]);
    }

    #[test]
    fn esr_and_okpo_lists() {
        assert_eq!(parse_esr_list("987303, 615004"), vec!["987303", "615004"]);
        let (all, okpo, names) = parse_parties("00111, 222", "ООО Первый, ООО Второй");
        assert!(!all);
        assert_eq!(okpo, vec!["111", "222"]);
        assert_eq!(names.len(), 2);
        let (all, _, _) = parse_parties("All", "все грузополучатели");
        assert!(all);
    }

    #[test]
    fn wash_repair_reserve_kept_when_active() {
        let wash = SAMPLE_4702
            .replace(r#""type": "Other""#, r#""type": "WashingStation""#)
            .replace(r#""date_end": "2026-03-04""#, r#""date_end": "3000-01-01""#);
        let mut raw = HashMap::new();
        raw.insert("w".into(), wash);
        raw.insert(
            "r".into(),
            tel_json("Others", "2026-01-01", "3000-01-01", "Все станции СКВ", "Все станции", "All")
                .replace(r#""type": "Other""#, r#""type": "RepairStation""#),
        );
        raw.insert(
            "s".into(),
            tel_json("Empty", "2026-01-01", "3000-01-01", "Все станции СКВ", "Все станции", "All")
                .replace(r#""type": "Other""#, r#""type": "ReserveStation""#),
        );
        let load = load_of(&raw);
        assert_eq!(load.stats.active, 3);
        assert_eq!(load.stats.active_service, 3);
        let wash = load
            .active
            .iter()
            .find(|p| p.convention_info == ConventionStatus::WashingStation)
            .expect("wash");
        assert_eq!(wash.cargo_class, ConventionCargoClass::All);
        let repair = load
            .active
            .iter()
            .find(|p| p.convention_info == ConventionStatus::RepairStation)
            .expect("repair");
        assert_eq!(repair.cargo_class, ConventionCargoClass::Empty);
        assert!(load
            .active
            .iter()
            .any(|p| p.convention_info == ConventionStatus::ReserveStation));
    }

    #[test]
    fn expired_wash_is_not_loaded() {
        let json = SAMPLE_4702.replace(r#""type": "Other""#, r#""type": "WashingStation""#);
        let mut raw = HashMap::new();
        raw.insert("w".into(), json);
        let load = load_of(&raw);
        assert_eq!(load.stats.expired, 1);
        assert_eq!(load.stats.active, 0);
        assert_eq!(load.stats.active_service, 0);
    }

    #[test]
    fn bad_json_counted() {
        let mut raw = HashMap::new();
        raw.insert("x".into(), "{not json".into());
        let load = load_of(&raw);
        assert_eq!(load.stats.bad_json, 1);
        assert_eq!(load.stats.parsed_ok, 0);
    }

    #[test]
    fn not_yet_started_skipped() {
        let mut raw = HashMap::new();
        raw.insert(
            "1".into(),
            tel_json("Empty", "2026-12-01", "3000-01-01", "Все станции СКВ", "Все станции", "All"),
        );
        let load = load_of(&raw);
        assert_eq!(load.stats.not_yet, 1);
        assert_eq!(load.stats.active, 0);
    }

    #[test]
    fn active_empty_on_skv_within_horizon() {
        let mut raw = HashMap::new();
        raw.insert(
            "e".into(),
            tel_json("Empty", "2026-09-10", "2026-09-20", "Все станции СКВ", "Все станции", "All"),
        );
        let load = load_of(&raw);
        assert_eq!(load.stats.active, 1);
        assert_eq!(load.active[0].cargo_class, ConventionCargoClass::Empty);
        assert_eq!(load.active[0].dest_railways, vec!["СКВ"]);
    }
}

//! Узлы отстоя (резервы) со стороны спроса: ёмкости для размещения излишка
//! порожних вагонов (эндпойнт `GetFreeReserveCapacityData`, см. `free_reserves.py`).
//!
//! Модуль назван `demand_reserves`, поскольку резервы здесь выступают «спросом»
//! на излишек вагонов. В дальнейшем появится supply-сторона отстоя — вагоны,
//! уже стоящие в резерве, которые можно выводить под заявки клиентов.
//!
//! Обработка ответа АПИ:
//! - дедупликация записей (аналог `drop_duplicates` в pandas) по всем значимым
//!   полям, кроме `DateReserveCapacity` (метка времени снимка);
//! - фильтры: `AgreementReserveCapacity > 0`, непустые код станции и дорога,
//!   договор активен на текущую дату (`DateBeg ≤ сегодня ≤ DateEnd`;
//!   нераспарсенные даты считаются активными, чтобы не терять ёмкость).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{NaiveDate, NaiveDateTime, Utc};
use rusqlite::{params, Connection};
use serde::Deserialize;

use crate::node::ReserveNode;

use super::client::{ApiClient, ApiEndpoint, ApiError};
use super::esr::normalize_esr6;
use super::StationRef;

// ---------------------------------------------------------------------------
// Структуры ответа АПИ
// ---------------------------------------------------------------------------

/// Код, который АПИ может прислать строкой или числом.
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
enum CodeValue {
    Str(String),
    Num(i64),
}

impl CodeValue {
    fn into_string(self) -> String {
        match self {
            Self::Str(s) => s.trim().to_string(),
            Self::Num(n) => n.to_string(),
        }
    }
}

/// Один элемент ответа `GetFreeReserveCapacityData`.
#[derive(Deserialize, Debug, Clone)]
struct FreeReserveApiItem {
    #[serde(rename = "RailWayReserveDivision", default)]
    division: Option<String>,
    #[serde(rename = "RailWayReserve", default)]
    railway: Option<String>,
    #[serde(rename = "RailWayReserveCode", default)]
    railway_code: Option<CodeValue>,
    #[serde(rename = "StationReserve", default)]
    station: Option<String>,
    #[serde(rename = "StationReserveCode", default)]
    station_code: Option<CodeValue>,
    #[serde(rename = "ApprovementDocNumber", default)]
    approvement_doc: Option<String>,
    #[serde(rename = "EtranId", default)]
    etran_id: Option<i64>,
    #[serde(rename = "ReserveOwner", default)]
    owner: Option<String>,
    #[serde(rename = "ReserveOwnerOKPO", default)]
    owner_okpo: Option<String>,
    #[serde(rename = "AgreementNumber", default)]
    agreement_number: Option<String>,
    #[serde(rename = "DateBeg", default)]
    date_beg: Option<String>,
    #[serde(rename = "DateEnd", default)]
    date_end: Option<String>,
    /// Согласованная ёмкость отстоя по договору — используется как ёмкость узла.
    #[serde(rename = "AgreementReserveCapacity", default)]
    agreement_capacity: Option<f64>,
}

impl FreeReserveApiItem {
    /// Ключ дедупликации: все значимые поля записи
    /// (без `DateReserveCapacity` — метки времени снимка).
    fn dedup_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.division.as_deref().unwrap_or(""),
            self.railway.as_deref().unwrap_or(""),
            self.railway_code.clone().map(CodeValue::into_string).unwrap_or_default(),
            self.station.as_deref().unwrap_or(""),
            self.station_code.clone().map(CodeValue::into_string).unwrap_or_default(),
            self.approvement_doc.as_deref().unwrap_or(""),
            self.etran_id.map(|v| v.to_string()).unwrap_or_default(),
            self.owner.as_deref().unwrap_or(""),
            self.owner_okpo.as_deref().unwrap_or(""),
            self.agreement_number.as_deref().unwrap_or(""),
            self.date_beg.as_deref().unwrap_or(""),
            self.date_end.as_deref().unwrap_or(""),
            self.agreement_capacity.map(|v| v.to_string()).unwrap_or_default(),
        )
    }
}

// ---------------------------------------------------------------------------
// Узлы отстоя
// ---------------------------------------------------------------------------

/// Результат загрузки резервов: узлы [`ReserveNode`] + статистика для логов.
#[derive(Debug, Clone, Default)]
pub struct ReserveData {
    pub nodes: Vec<ReserveNode>,
    /// Записей в ответе АПИ до обработки.
    pub raw_records: usize,
    /// Отброшено дубликатов.
    pub duplicates: usize,
    /// Отброшено фильтрами (ёмкость, пустые коды, неактивный договор).
    pub filtered: usize,
    /// Отброшено как «чужие» по ban-list владельцев (`reserve_owners.json`):
    /// пара (код станции, ОКПО) присутствует в справочнике запрещённых. 0, если
    /// ban-list пуст (фильтр по владельцам отключён).
    pub foreign_filtered: usize,
}

impl ReserveData {
    pub fn total_capacity(&self) -> i32 {
        self.nodes.iter().map(|n| n.capacity).sum()
    }
}

// ---------------------------------------------------------------------------
// Построение узлов
// ---------------------------------------------------------------------------

fn parse_reserve_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim().trim_end_matches('Z');
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
        .map(|d| d.date())
        .ok()
        .or_else(|| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

/// Договор активен на `today`. Нераспарсенная или отсутствующая граница не ограничивает.
fn agreement_active(item: &FreeReserveApiItem, today: NaiveDate) -> bool {
    if let Some(beg) = item.date_beg.as_deref().and_then(parse_reserve_date) {
        if today < beg {
            return false;
        }
    }
    if let Some(end) = item.date_end.as_deref().and_then(parse_reserve_date) {
        if today > end {
            return false;
        }
    }
    true
}

/// Дедуплицирует и фильтрует записи АПИ, строит узлы отстоя.
fn build_reserve_nodes(items: Vec<FreeReserveApiItem>, today: NaiveDate) -> ReserveData {
    let raw_records = items.len();
    let mut seen: HashSet<String> = HashSet::new();
    let mut duplicates = 0_usize;
    let mut filtered = 0_usize;
    let mut nodes: Vec<ReserveNode> = Vec::new();

    for item in items {
        if !seen.insert(item.dedup_key()) {
            duplicates += 1;
            continue;
        }

        let capacity = item.agreement_capacity.unwrap_or(0.0).round() as i32;
        let station_code = item
            .station_code
            .clone()
            .map(CodeValue::into_string)
            .unwrap_or_default();
        let railway_short = item.railway.as_deref().unwrap_or("").trim().to_string();

        if capacity <= 0
            || station_code.is_empty()
            || railway_short.is_empty()
            || !agreement_active(&item, today)
        {
            filtered += 1;
            continue;
        }

        nodes.push(ReserveNode {
            r_id: nodes.len() + 1,
            station_name: item.station.as_deref().unwrap_or("").trim().to_string(),
            station_code,
            railway_short,
            railway_code: item
                .railway_code
                .map(CodeValue::into_string)
                .filter(|s| !s.is_empty()),
            division: item.division.clone().filter(|s| !s.trim().is_empty()),
            owner: item.owner.clone().filter(|s| !s.trim().is_empty()),
            owner_okpo: item.owner_okpo.clone().filter(|s| !s.trim().is_empty()),
            agreement_number: item
                .agreement_number
                .clone()
                .filter(|s| !s.trim().is_empty()),
            capacity,
        });
    }

    ReserveData { nodes, raw_records, duplicates, filtered, foreign_filtered: 0 }
}

/// Пара-ключ ban-list для записи: `(код станции ЕСР-6, ОКПО владельца)`.
///
/// Нормализация идентична [`load_reserve_owners_banlist`]: код станции → 6 цифр,
/// ОКПО → trim. Пустые значения дают пустые строки.
fn owner_filter_key(item: &FreeReserveApiItem) -> (String, String) {
    let station_code = item
        .station_code
        .clone()
        .map(CodeValue::into_string)
        .map(|s| normalize_esr6(&s))
        .unwrap_or_default();
    let owner_okpo = item
        .owner_okpo
        .as_deref()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    (station_code, owner_okpo)
}

/// Уникальные станции отстоя для запроса тарифов (по образцу `wash_station_refs`).
pub fn reserve_station_refs(nodes: &[ReserveNode]) -> Vec<StationRef> {
    let mut set: HashSet<(String, String)> = HashSet::new();
    for n in nodes {
        set.insert((n.station_code.clone(), n.railway_short.clone()));
    }
    let mut v: Vec<_> = set
        .into_iter()
        .map(|(code, rw)| StationRef::new(code, rw))
        .collect();
    v.sort_by(|a, b| {
        a.station_code
            .cmp(&b.station_code)
            .then_with(|| a.railway_short_name.cmp(&b.railway_short_name))
    });
    v
}

// ---------------------------------------------------------------------------
// Методы ApiClient
// ---------------------------------------------------------------------------

impl ApiClient {
    /// Запрашивает сырые записи ёмкостей отстоя (`GetFreeReserveCapacityData`).
    ///
    /// Дедупликация и фильтрация выполняются позже: при upsert в БД ключом служит
    /// `etran_id` (дубли снимка схлопываются по PK), а при чтении — [`build_reserve_nodes`].
    async fn fetch_reserve_permits(&self) -> Result<Vec<FreeReserveApiItem>, ApiError> {
        let url = ApiEndpoint::FreeReserves.url(&self.base_url);
        let response = self.client.get(&url).send().await?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::UnexpectedStatus { status: status.as_u16(), body });
        }

        Ok(response.json::<Vec<FreeReserveApiItem>>().await?)
    }

    /// Запрашивает свободные ёмкости отстоя (`GetFreeReserveCapacityData`)
    /// и строит узлы отстоя на текущую дату.
    pub async fn fetch_reserve_nodes(&self) -> Result<ReserveData, ApiError> {
        let items = self.fetch_reserve_permits().await?;
        Ok(build_reserve_nodes(items, Utc::now().date_naive()))
    }
}

// ---------------------------------------------------------------------------
// Накопительная SQLite-БД разрешений на отстой
// ---------------------------------------------------------------------------

/// Путь к БД отстоя по умолчанию (накопительная, не коммитится в git).
pub const DEFAULT_RESERVES_DB_PATH: &str = "data/reserves/reserves.sqlite";

/// Путь к файлу БД отстоя: env `RESERVES_DB` или [`DEFAULT_RESERVES_DB_PATH`].
pub fn reserves_db_path() -> PathBuf {
    std::env::var("RESERVES_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_RESERVES_DB_PATH))
}

const CREATE_TABLE_SQL: &str = "
CREATE TABLE IF NOT EXISTS reserve_permits (
    etran_id            INTEGER PRIMARY KEY,
    division            TEXT,
    railway             TEXT,
    railway_code        TEXT,
    station             TEXT,
    station_code        TEXT,
    approvement_doc     TEXT,
    owner               TEXT,
    owner_okpo          TEXT,
    agreement_number    TEXT,
    date_beg            TEXT,
    date_end            TEXT,
    agreement_capacity  REAL,
    synced_at           TEXT NOT NULL
);
";

const UPSERT_SQL: &str = "
INSERT INTO reserve_permits (
    etran_id, division, railway, railway_code, station, station_code,
    approvement_doc, owner, owner_okpo, agreement_number,
    date_beg, date_end, agreement_capacity, synced_at
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
ON CONFLICT(etran_id) DO UPDATE SET
    division           = excluded.division,
    railway            = excluded.railway,
    railway_code       = excluded.railway_code,
    station            = excluded.station,
    station_code       = excluded.station_code,
    approvement_doc    = excluded.approvement_doc,
    owner              = excluded.owner,
    owner_okpo         = excluded.owner_okpo,
    agreement_number   = excluded.agreement_number,
    date_beg           = excluded.date_beg,
    date_end           = excluded.date_end,
    agreement_capacity = excluded.agreement_capacity,
    synced_at          = excluded.synced_at;
";

const SELECT_SQL: &str = "
SELECT division, railway, railway_code, station, station_code, approvement_doc,
       etran_id, owner, owner_okpo, agreement_number, date_beg, date_end,
       agreement_capacity
FROM reserve_permits;
";

/// Статистика синхронизации БД отстоя для логов.
#[derive(Debug, Clone, Default)]
pub struct ReserveSyncStats {
    /// Записей получено из API.
    pub fetched: usize,
    /// Записей записано/обновлено в БД (по `etran_id`).
    pub upserted: usize,
    /// Пропущено записей без `etran_id` (ключ накопления отсутствует).
    pub skipped_no_etran: usize,
}

/// Открывает (создаёт при отсутствии) БД отстоя и гарантирует наличие таблицы.
pub fn open_reserves_db(path: impl AsRef<Path>) -> anyhow::Result<Connection> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("создание каталога {}", parent.display()))?;
        }
    }
    let conn = Connection::open(path)
        .with_context(|| format!("открытие БД отстоя {}", path.display()))?;
    conn.execute_batch(CREATE_TABLE_SQL)
        .context("создание таблицы reserve_permits")?;
    Ok(conn)
}

/// Запрашивает свежие разрешения из API и складывает их в БД (upsert по `etran_id`).
///
/// Записи без `etran_id` пропускаются (нет стабильного ключа накопления). Ошибка
/// API распространяется наверх — вызывающий решает, фатально это или нет (в батче —
/// нет: используются ранее накопленные данные БД).
pub async fn sync_reserves_to_db(
    client: &ApiClient,
    conn: &Connection,
) -> anyhow::Result<ReserveSyncStats> {
    let items = client
        .fetch_reserve_permits()
        .await
        .context("запрос ёмкостей отстоя из API")?;
    upsert_permits(conn, &items)
}

/// Upsert набора записей в БД отстоя (выделено для тестов без сети).
fn upsert_permits(conn: &Connection, items: &[FreeReserveApiItem]) -> anyhow::Result<ReserveSyncStats> {
    let fetched = items.len();
    let now = Utc::now().to_rfc3339();
    let mut upserted = 0_usize;
    let mut skipped_no_etran = 0_usize;

    let tx = conn.unchecked_transaction().context("транзакция БД отстоя")?;
    {
        let mut stmt = tx.prepare(UPSERT_SQL).context("подготовка upsert отстоя")?;
        for item in items {
            let Some(etran_id) = item.etran_id else {
                skipped_no_etran += 1;
                continue;
            };
            stmt.execute(params![
                etran_id,
                item.division.as_deref(),
                item.railway.as_deref(),
                item.railway_code.clone().map(CodeValue::into_string),
                item.station.as_deref(),
                item.station_code.clone().map(CodeValue::into_string),
                item.approvement_doc.as_deref(),
                item.owner.as_deref(),
                item.owner_okpo.as_deref(),
                item.agreement_number.as_deref(),
                item.date_beg.as_deref(),
                item.date_end.as_deref(),
                item.agreement_capacity,
                now,
            ])
            .context("upsert записи отстоя")?;
            upserted += 1;
        }
    }
    tx.commit().context("commit БД отстоя")?;

    Ok(ReserveSyncStats { fetched, upserted, skipped_no_etran })
}

fn row_to_item(row: &rusqlite::Row) -> rusqlite::Result<FreeReserveApiItem> {
    Ok(FreeReserveApiItem {
        division: row.get::<_, Option<String>>(0)?,
        railway: row.get::<_, Option<String>>(1)?,
        railway_code: row.get::<_, Option<String>>(2)?.map(CodeValue::Str),
        station: row.get::<_, Option<String>>(3)?,
        station_code: row.get::<_, Option<String>>(4)?.map(CodeValue::Str),
        approvement_doc: row.get::<_, Option<String>>(5)?,
        etran_id: row.get::<_, Option<i64>>(6)?,
        owner: row.get::<_, Option<String>>(7)?,
        owner_okpo: row.get::<_, Option<String>>(8)?,
        agreement_number: row.get::<_, Option<String>>(9)?,
        date_beg: row.get::<_, Option<String>>(10)?,
        date_end: row.get::<_, Option<String>>(11)?,
        agreement_capacity: row.get::<_, Option<f64>>(12)?,
    })
}

/// Читает накопленные разрешения из БД и строит активные на `today` узлы отстоя.
///
/// Фильтрация (ёмкость, пустые коды, активность договора по `date_beg`/`date_end`)
/// выполняется тем же [`build_reserve_nodes`], что и для прямого ответа API.
///
/// Дополнительно применяется ban-list «чужих» владельцев `owners_banlist`
/// (`reserve_owners.json`): отбрасываются записи, у которых пара
/// `(код станции, ОКПО владельца)` присутствует в справочнике запрещённых —
/// «чужие» ёмкости отстоя исключаются (счётчик [`ReserveData::foreign_filtered`]).
/// Если ban-list **пуст** (справочник не загружен), фильтр по владельцам отключается
/// и проходят все записи.
pub fn load_active_reserve_nodes(
    conn: &Connection,
    today: NaiveDate,
    owners_banlist: &HashSet<(String, String)>,
) -> anyhow::Result<ReserveData> {
    let mut stmt = conn.prepare(SELECT_SQL).context("подготовка чтения отстоя")?;
    let items = stmt
        .query_map([], row_to_item)
        .context("чтение записей отстоя")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("разбор записей отстоя")?;

    let (items, foreign_filtered) = if owners_banlist.is_empty() {
        (items, 0)
    } else {
        let before = items.len();
        let kept: Vec<FreeReserveApiItem> = items
            .into_iter()
            .filter(|item| !owners_banlist.contains(&owner_filter_key(item)))
            .collect();
        let dropped = before - kept.len();
        (kept, dropped)
    };

    let mut data = build_reserve_nodes(items, today);
    data.foreign_filtered = foreign_filtered;
    Ok(data)
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_items(json: &str) -> Vec<FreeReserveApiItem> {
        serde_json::from_str(json).expect("valid test json")
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 6, 11).unwrap()
    }

    /// Полные дубликаты схлопываются (DateReserveCapacity не входит в ключ).
    #[test]
    fn deduplicates_records() {
        let items = parse_items(
            r#"[
                {"DateReserveCapacity": "2026-06-11T09:00:00.000Z",
                 "RailWayReserve": "МСК", "RailWayReserveCode": 17,
                 "StationReserve": "Отстойная", "StationReserveCode": "123456",
                 "ReserveOwner": "ООО Ромашка", "AgreementNumber": "Д-1",
                 "AgreementReserveCapacity": 50},
                {"DateReserveCapacity": "2026-06-11T10:30:00.000Z",
                 "RailWayReserve": "МСК", "RailWayReserveCode": 17,
                 "StationReserve": "Отстойная", "StationReserveCode": "123456",
                 "ReserveOwner": "ООО Ромашка", "AgreementNumber": "Д-1",
                 "AgreementReserveCapacity": 50},
                {"RailWayReserve": "МСК", "RailWayReserveCode": 17,
                 "StationReserve": "Отстойная", "StationReserveCode": "123456",
                 "ReserveOwner": "ООО Ромашка", "AgreementNumber": "Д-2",
                 "AgreementReserveCapacity": 30}
            ]"#,
        );
        let data = build_reserve_nodes(items, today());
        assert_eq!(data.raw_records, 3);
        assert_eq!(data.duplicates, 1);
        assert_eq!(data.nodes.len(), 2);
        assert_eq!(data.total_capacity(), 80);
        assert_eq!(data.nodes[0].railway_code.as_deref(), Some("17"));
    }

    /// Нулевая ёмкость и пустой код станции отбрасываются фильтрами.
    #[test]
    fn filters_capacity_and_empty_codes() {
        let items = parse_items(
            r#"[
                {"RailWayReserve": "МСК", "StationReserve": "А",
                 "StationReserveCode": "111111", "AgreementReserveCapacity": 0},
                {"RailWayReserve": "МСК", "StationReserve": "Б",
                 "StationReserveCode": "", "AgreementReserveCapacity": 10},
                {"StationReserve": "В", "StationReserveCode": "222222",
                 "AgreementReserveCapacity": 10},
                {"RailWayReserve": "МСК", "StationReserve": "Г",
                 "StationReserveCode": "333333", "AgreementReserveCapacity": 10}
            ]"#,
        );
        let data = build_reserve_nodes(items, today());
        assert_eq!(data.filtered, 3);
        assert_eq!(data.nodes.len(), 1);
        assert_eq!(data.nodes[0].station_name, "Г");
    }

    /// Истёкший и ещё не начавшийся договоры отбрасываются;
    /// нераспарсенные даты считаются активными.
    #[test]
    fn filters_inactive_agreements() {
        let items = parse_items(
            r#"[
                {"RailWayReserve": "МСК", "StationReserve": "Истёк",
                 "StationReserveCode": "111111", "AgreementReserveCapacity": 10,
                 "DateBeg": "2026-01-01T00:00:00.000Z", "DateEnd": "2026-06-10T00:00:00.000Z"},
                {"RailWayReserve": "МСК", "StationReserve": "Будущий",
                 "StationReserveCode": "222222", "AgreementReserveCapacity": 10,
                 "DateBeg": "2026-07-01T00:00:00.000Z", "DateEnd": "2026-12-31T00:00:00.000Z"},
                {"RailWayReserve": "МСК", "StationReserve": "Активный",
                 "StationReserveCode": "333333", "AgreementReserveCapacity": 10,
                 "DateBeg": "2026-06-01T00:00:00.000Z", "DateEnd": "2026-06-30T00:00:00.000Z"},
                {"RailWayReserve": "МСК", "StationReserve": "БезДат",
                 "StationReserveCode": "444444", "AgreementReserveCapacity": 10,
                 "DateBeg": "не дата", "DateEnd": null}
            ]"#,
        );
        let data = build_reserve_nodes(items, today());
        let names: Vec<&str> = data.nodes.iter().map(|n| n.station_name.as_str()).collect();
        assert_eq!(names, vec!["Активный", "БезДат"]);
        assert_eq!(data.filtered, 2);
    }

    /// Станции для тарифов уникальны по (код, дорога) и отсортированы.
    #[test]
    fn station_refs_unique_sorted() {
        let items = parse_items(
            r#"[
                {"RailWayReserve": "МСК", "StationReserve": "А",
                 "StationReserveCode": "222222", "AgreementReserveCapacity": 5,
                 "AgreementNumber": "Д-1"},
                {"RailWayReserve": "МСК", "StationReserve": "А",
                 "StationReserveCode": "222222", "AgreementReserveCapacity": 7,
                 "AgreementNumber": "Д-2"},
                {"RailWayReserve": "ЮВС", "StationReserve": "Б",
                 "StationReserveCode": "111111", "AgreementReserveCapacity": 3,
                 "AgreementNumber": "Д-3"}
            ]"#,
        );
        let data = build_reserve_nodes(items, today());
        assert_eq!(data.nodes.len(), 3);
        let refs = reserve_station_refs(&data.nodes);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].station_code, "111111");
        assert_eq!(refs[1].station_code, "222222");
    }

    // -----------------------------------------------------------------------
    // Тесты накопительной БД
    // -----------------------------------------------------------------------

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(CREATE_TABLE_SQL).unwrap();
        conn
    }

    /// Повторный снимок с тем же etran_id не плодит строки, а обновляет ёмкость.
    #[test]
    fn upsert_updates_capacity_by_etran_id() {
        let conn = mem_db();
        let snap1 = parse_items(
            r#"[
                {"RailWayReserve": "МСК", "StationReserve": "Отстойная",
                 "StationReserveCode": "123456", "EtranId": 100,
                 "AgreementReserveCapacity": 50,
                 "DateBeg": "2026-06-01T00:00:00.000Z", "DateEnd": "2026-12-31T00:00:00.000Z"}
            ]"#,
        );
        let s1 = upsert_permits(&conn, &snap1).unwrap();
        assert_eq!((s1.fetched, s1.upserted, s1.skipped_no_etran), (1, 1, 0));

        let snap2 = parse_items(
            r#"[
                {"RailWayReserve": "МСК", "StationReserve": "Отстойная",
                 "StationReserveCode": "123456", "EtranId": 100,
                 "AgreementReserveCapacity": 80,
                 "DateBeg": "2026-06-01T00:00:00.000Z", "DateEnd": "2026-12-31T00:00:00.000Z"}
            ]"#,
        );
        upsert_permits(&conn, &snap2).unwrap();

        let data = load_active_reserve_nodes(&conn, today(), &HashSet::new()).unwrap();
        assert_eq!(data.nodes.len(), 1);
        assert_eq!(data.total_capacity(), 80);
    }

    /// При чтении из БД истёкшие по date_end разрешения отбрасываются.
    #[test]
    fn load_filters_expired_date_end() {
        let conn = mem_db();
        let items = parse_items(
            r#"[
                {"RailWayReserve": "МСК", "StationReserve": "Истёк",
                 "StationReserveCode": "111111", "EtranId": 1,
                 "AgreementReserveCapacity": 10,
                 "DateBeg": "2026-01-01T00:00:00.000Z", "DateEnd": "2026-06-10T00:00:00.000Z"},
                {"RailWayReserve": "МСК", "StationReserve": "Активный",
                 "StationReserveCode": "222222", "EtranId": 2,
                 "AgreementReserveCapacity": 10,
                 "DateBeg": "2026-06-01T00:00:00.000Z", "DateEnd": "2026-06-30T00:00:00.000Z"},
                {"RailWayReserve": "МСК", "StationReserve": "БезДат",
                 "StationReserveCode": "333333", "EtranId": 3,
                 "AgreementReserveCapacity": 10}
            ]"#,
        );
        let s = upsert_permits(&conn, &items).unwrap();
        assert_eq!(s.upserted, 3);

        let data = load_active_reserve_nodes(&conn, today(), &HashSet::new()).unwrap();
        let mut names: Vec<&str> = data.nodes.iter().map(|n| n.station_name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["Активный", "БезДат"]);
    }

    /// Записи без etran_id не попадают в БД и считаются как skipped_no_etran.
    #[test]
    fn upsert_skips_null_etran_id() {
        let conn = mem_db();
        let items = parse_items(
            r#"[
                {"RailWayReserve": "МСК", "StationReserve": "БезEtran",
                 "StationReserveCode": "444444", "AgreementReserveCapacity": 10}
            ]"#,
        );
        let s = upsert_permits(&conn, &items).unwrap();
        assert_eq!((s.fetched, s.upserted, s.skipped_no_etran), (1, 0, 1));

        let data = load_active_reserve_nodes(&conn, today(), &HashSet::new()).unwrap();
        assert_eq!(data.nodes.len(), 0);
    }

    /// Непустой ban-list отбрасывает «чужие» (станция+ОКПО), остальные оставляет.
    #[test]
    fn load_drops_foreign_owners_by_banlist() {
        let conn = mem_db();
        let items = parse_items(
            r#"[
                {"RailWayReserve": "СВР", "StationReserve": "Своя 1",
                 "StationReserveCode": "769001", "EtranId": 1,
                 "ReserveOwnerOKPO": "11111111", "AgreementReserveCapacity": 10},
                {"RailWayReserve": "СВР", "StationReserve": "Своя 2",
                 "StationReserveCode": "769002", "EtranId": 2,
                 "ReserveOwnerOKPO": "22222222", "AgreementReserveCapacity": 20},
                {"RailWayReserve": "СВР", "StationReserve": "Чужая",
                 "StationReserveCode": "769002", "EtranId": 3,
                 "ReserveOwnerOKPO": "00203944", "AgreementReserveCapacity": 30}
            ]"#,
        );
        upsert_permits(&conn, &items).unwrap();

        // Запрещаем только пару 769002 + 00203944.
        let mut ban = HashSet::new();
        ban.insert(("769002".to_string(), "00203944".to_string()));

        let data = load_active_reserve_nodes(&conn, today(), &ban).unwrap();
        assert_eq!(data.nodes.len(), 2);
        assert_eq!(data.foreign_filtered, 1);
        assert_eq!(data.total_capacity(), 30);
        // Станция 769002 со «своим» ОКПО осталась, запрещённая пара ушла.
        assert!(data
            .nodes
            .iter()
            .all(|n| n.owner_okpo.as_deref() != Some("00203944")));
    }

    /// Пустой ban-list отключает фильтр по владельцам — проходят все записи.
    #[test]
    fn empty_banlist_keeps_all_owners() {
        let conn = mem_db();
        let items = parse_items(
            r#"[
                {"RailWayReserve": "СВР", "StationReserve": "Своя",
                 "StationReserveCode": "769002", "EtranId": 1,
                 "ReserveOwnerOKPO": "00203944", "AgreementReserveCapacity": 10},
                {"RailWayReserve": "СВР", "StationReserve": "Чужая",
                 "StationReserveCode": "769002", "EtranId": 2,
                 "ReserveOwnerOKPO": "99999999", "AgreementReserveCapacity": 10}
            ]"#,
        );
        upsert_permits(&conn, &items).unwrap();

        let data = load_active_reserve_nodes(&conn, today(), &HashSet::new()).unwrap();
        assert_eq!(data.nodes.len(), 2);
        assert_eq!(data.foreign_filtered, 0);
    }
}

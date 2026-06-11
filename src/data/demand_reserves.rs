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

use chrono::{NaiveDate, NaiveDateTime, Utc};
use serde::Deserialize;

use crate::node::ReserveNode;

use super::client::{ApiClient, ApiEndpoint, ApiError};
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

    ReserveData { nodes, raw_records, duplicates, filtered }
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
    /// Запрашивает свободные ёмкости отстоя (`GetFreeReserveCapacityData`)
    /// и строит узлы отстоя на текущую дату.
    pub async fn fetch_reserve_nodes(&self) -> Result<ReserveData, ApiError> {
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

        let items = response.json::<Vec<FreeReserveApiItem>>().await?;
        Ok(build_reserve_nodes(items, Utc::now().date_naive()))
    }
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
}

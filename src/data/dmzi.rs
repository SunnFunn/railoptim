//! Загрузка ограничений ДМЗИ (динамическая модель загрузки ж-д инфраструктуры).
//!
//! РЖД ежедневно публикует на 7 дней вперёд лимиты количества вагонов, которые
//! можно направить на каждую дорогу (эндпойнт `GetDMZIData`, см. `dmzi.py`).
//! Из ответа берутся только записи `NormativType == "Ostatok"`.
//!
//! Правила свёртки лимитов по дороге (поле `Normativ`):
//! - **период 1** (вагоны из АПИ, готовы сегодня) — **сумма** `Normativ`
//!   за первые 5 суток от текущей даты (смещения 0–4);
//! - **период 10** (дислокация 2–10 суток) — **сумма** `Normativ`
//!   за оставшиеся сутки окна (смещение 5; смещения 3–4 отходят периоду 1).
//!
//! Код дороги — префикс `DMZIRailWayGroup` до `/` (например, `"МСК/ЗНВ"` → `МСК`);
//! матчится с `DemandNode::railway_name` (дорога погрузки) после [`normalize_railway`].

use std::collections::HashMap;

use chrono::{NaiveDate, NaiveDateTime, Utc};
use serde::Deserialize;

use super::client::{ApiClient, ApiEndpoint, ApiError};

// ---------------------------------------------------------------------------
// Константы запроса
// ---------------------------------------------------------------------------

/// Горизонт ДМЗИ: `DateEnd = DateBegin + 6` суток (7 дней включительно).
pub const DMZI_HORIZON_DAYS: i64 = 6;

/// Окно суммирования `Normativ` для периода 1: первые 5 суток
/// (смещение даты норматива от текущей даты, в сутках).
const P1_SUM_OFFSET_DAYS: std::ops::RangeInclusive<i64> = 0..=4;

/// Окно суммирования `Normativ` для периода 10: 4-е, 5-е и 6-е сутки.
///
/// Пересекается с [`P1_SUM_OFFSET_DAYS`] на смещениях 3–4: при пересечении
/// приоритет у периода 1 (см. ветвление в [`aggregate`]), поэтому фактически
/// в период 10 попадает только смещение 5.
const P10_SUM_OFFSET_DAYS: std::ops::RangeInclusive<i64> = 3..=5;

/// Тип подвижного состава: зерновозы.
pub const DMZI_CAR_KIND_GRAIN: &str = "20";

/// Вид норматива: заадресовка (подсыл порожних вагонов на дорогу).
pub const DMZI_NORMATIVE_ZAADRESOVKA: &str = "6";

// ---------------------------------------------------------------------------
// Структуры
// ---------------------------------------------------------------------------

/// Один элемент ответа `GetDMZIData`.
#[derive(Deserialize, Debug)]
struct DmziApiItem {
    /// `"МСК/ЗНВ"` — дорога / тип подвижного состава.
    #[serde(rename = "DMZIRailWayGroup", default)]
    railway_group: Option<String>,

    /// `"Ostatok"` — остаток норматива (только такие записи учитываются).
    #[serde(rename = "NormativType", default)]
    normativ_type: Option<String>,

    /// Дата действия норматива, `"2026-06-13T00:00:00"`.
    #[serde(rename = "DateOfNormativ", default)]
    date_of_normativ: Option<String>,

    /// Лимит вагонов (верхняя граница подсыла на дорогу в эту дату).
    #[serde(rename = "Normativ", default)]
    normativ: Option<f64>,
}

/// Свёрнутые квоты ДМЗИ одной дороги.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmziRailwayQuota {
    /// Лимит на подсыл вагонов периода 1: сумма `Normativ` за сутки 1–5 (смещения 0–4).
    pub limit_p1: i32,
    /// Лимит на подсыл вагонов периода 10: сумма `Normativ` за 6-е сутки (смещение 5).
    pub limit_p10: i32,
}

/// Квоты ДМЗИ по дорогам: нормализованный 3-буквенный код дороги → лимиты.
#[derive(Debug, Clone, Default)]
pub struct DmziQuotas {
    pub by_railway: HashMap<String, DmziRailwayQuota>,
    /// Количество учтённых записей `Ostatok` в ответе АПИ (для логов).
    pub records: usize,
}

impl DmziQuotas {
    pub fn is_empty(&self) -> bool {
        self.by_railway.is_empty()
    }

    /// Карта бакетов для решателя: `(код дороги, период предложения 1|10)` → лимит.
    ///
    /// Тип совпадает с `solver::DmziLimits`.
    pub fn to_limits(&self) -> HashMap<(String, u8), i32> {
        let mut limits = HashMap::with_capacity(self.by_railway.len() * 2);
        for (railway, q) in &self.by_railway {
            limits.insert((railway.clone(), 1), q.limit_p1);
            limits.insert((railway.clone(), 10), q.limit_p10);
        }
        limits
    }
}

/// Нормализация кода дороги для сопоставления ДМЗИ ↔ узлы спроса.
pub fn normalize_railway(name: &str) -> String {
    name.trim().to_uppercase()
}

// ---------------------------------------------------------------------------
// Агрегация ответа
// ---------------------------------------------------------------------------

fn parse_dmzi_date(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim();
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
        })
}

/// Сворачивает записи ответа в квоты по дорогам.
///
/// Период 1 — сумма `Normativ` за смещения [`P1_SUM_OFFSET_DAYS`] от `today`;
/// период 10 — сумма за смещения [`P10_SUM_OFFSET_DAYS`]. Записи без распознанной
/// даты или вне обоих окон не учитываются.
fn aggregate(items: &[DmziApiItem], today: NaiveDate) -> DmziQuotas {
    let mut by_railway: HashMap<String, DmziRailwayQuota> = HashMap::new();
    let mut records = 0_usize;

    for item in items {
        let is_ostatok = item
            .normativ_type
            .as_deref()
            .map(|t| t.trim().eq_ignore_ascii_case("ostatok"))
            .unwrap_or(false);
        if !is_ostatok {
            continue;
        }
        let Some(group) = item.railway_group.as_deref() else {
            continue;
        };
        let railway = normalize_railway(group.split('/').next().unwrap_or(""));
        if railway.is_empty() {
            continue;
        }
        let Some(normativ) = item.normativ else {
            continue;
        };
        let normativ = normativ.round() as i32;
        if normativ < 0 {
            continue;
        }
        let Some(date) = item.date_of_normativ.as_deref().and_then(parse_dmzi_date) else {
            continue;
        };

        let offset = (date.date() - today).num_days();
        let is_p1 = P1_SUM_OFFSET_DAYS.contains(&offset);
        let is_p10 = P10_SUM_OFFSET_DAYS.contains(&offset);
        if !is_p1 && !is_p10 {
            // Вне обоих окон (например, 7-е сутки горизонта) — не учитывается,
            // запись по дороге не создаётся.
            continue;
        }

        let quota = by_railway
            .entry(railway)
            .or_insert(DmziRailwayQuota { limit_p1: 0, limit_p10: 0 });
        if is_p1 {
            quota.limit_p1 += normativ;
        } else {
            quota.limit_p10 += normativ;
        }
        records += 1;
    }

    DmziQuotas { by_railway, records }
}

// ---------------------------------------------------------------------------
// Методы ApiClient
// ---------------------------------------------------------------------------

impl ApiClient {
    /// Запрашивает ограничения ДМЗИ (`GetDMZIData`) на горизонт
    /// `[сегодня; сегодня + DMZI_HORIZON_DAYS]` и сворачивает их в [`DmziQuotas`].
    pub async fn fetch_dmzi_quotas(&self) -> Result<DmziQuotas, ApiError> {
        let today = Utc::now();
        let date_begin = today.format("%Y-%m-%d").to_string();
        let date_end = (today + chrono::Duration::days(DMZI_HORIZON_DAYS))
            .format("%Y-%m-%d")
            .to_string();

        let url = ApiEndpoint::Dmzi.url(&self.base_url);
        let response = self
            .client
            .get(&url)
            .query(&[
                ("DateBegin", date_begin.as_str()),
                ("DateEnd", date_end.as_str()),
                ("RailWayId", ""),
                ("CarKindId", DMZI_CAR_KIND_GRAIN),
                ("NormativeId", DMZI_NORMATIVE_ZAADRESOVKA),
            ])
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::UnexpectedStatus { status: status.as_u16(), body });
        }

        let items = response.json::<Vec<DmziApiItem>>().await?;
        Ok(aggregate(&items, today.date_naive()))
    }
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_items(json: &str) -> Vec<DmziApiItem> {
        serde_json::from_str(json).expect("valid test json")
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 6, 10).unwrap()
    }

    /// p1 — сумма Normativ за сутки 1–5 (10–14.06), p10 — только 6-е сутки (15.06),
    /// т.к. на пересечении окон (смещения 3–4) приоритет у периода 1;
    /// 7-е сутки (16.06) не учитываются; записи NormativType != Ostatok отбрасываются
    /// (регистр не важен).
    #[test]
    fn aggregate_sums_by_windows() {
        let items = parse_items(
            r#"[
                {"DMZIRailWayGroup": "МСК/ЗНВ", "NormativType": "Ostatok",
                 "DateOfNormativ": "2026-06-10T00:00:00", "Normativ": 25},
                {"DMZIRailWayGroup": "МСК/ЗНВ", "NormativType": "ostatok",
                 "DateOfNormativ": "2026-06-11T00:00:00", "Normativ": 10},
                {"DMZIRailWayGroup": "МСК/ЗНВ", "NormativType": "Ostatok",
                 "DateOfNormativ": "2026-06-12T00:00:00", "Normativ": 5},
                {"DMZIRailWayGroup": "МСК/ЗНВ", "NormativType": "Ostatok",
                 "DateOfNormativ": "2026-06-13T00:00:00", "Normativ": 40},
                {"DMZIRailWayGroup": "МСК/ЗНВ", "NormativType": "Ostatok",
                 "DateOfNormativ": "2026-06-15T00:00:00", "Normativ": 7},
                {"DMZIRailWayGroup": "МСК/ЗНВ", "NormativType": "Ostatok",
                 "DateOfNormativ": "2026-06-16T00:00:00", "Normativ": 999},
                {"DMZIRailWayGroup": "МСК/ЗНВ", "NormativType": "Plan",
                 "DateOfNormativ": "2026-06-10T00:00:00", "Normativ": 999},
                {"DMZIRailWayGroup": "ЮВС/ЗНВ", "NormativType": "Ostatok",
                 "DateOfNormativ": "2026-06-11T00:00:00", "Normativ": 7}
            ]"#,
        );
        let q = aggregate(&items, today());
        assert_eq!(q.records, 6);
        let msk = q.by_railway.get("МСК").expect("МСК");
        assert_eq!(msk.limit_p1, 25 + 10 + 5 + 40); // сутки 1–5 (вкл. 13.06, смещение 3)
        assert_eq!(msk.limit_p10, 7); // только 6-е сутки (15.06, смещение 5)
        let yvs = q.by_railway.get("ЮВС").expect("ЮВС");
        assert_eq!(yvs.limit_p1, 7);
        assert_eq!(yvs.limit_p10, 0); // записей на 6-е сутки нет
    }

    /// Записи без распознанной даты не учитываются; дорога только с такими
    /// записями не получает квоты (не ограничивается).
    #[test]
    fn aggregate_skips_undated_records() {
        let items = parse_items(
            r#"[
                {"DMZIRailWayGroup": "СКВ/ЗНВ", "NormativType": "Ostatok", "Normativ": 12},
                {"DMZIRailWayGroup": "СКВ/ЗНВ", "NormativType": "Ostatok", "Normativ": 30}
            ]"#,
        );
        let q = aggregate(&items, today());
        assert!(q.by_railway.is_empty());
        assert_eq!(q.records, 0);
    }

    /// Дорога с записями только вне окон (7-е сутки) не получает квоты.
    #[test]
    fn aggregate_skips_out_of_window_railway() {
        let items = parse_items(
            r#"[{"DMZIRailWayGroup": "ЗАБ/ЗНВ", "NormativType": "Ostatok",
                 "DateOfNormativ": "2026-06-16T00:00:00", "Normativ": 50}]"#,
        );
        let q = aggregate(&items, today());
        assert!(!q.by_railway.contains_key("ЗАБ"));
    }

    /// to_limits разворачивает квоты в бакеты (дорога, период).
    #[test]
    fn to_limits_buckets() {
        let mut by_railway = HashMap::new();
        by_railway.insert(
            "МСК".to_string(),
            DmziRailwayQuota { limit_p1: 25, limit_p10: 40 },
        );
        let q = DmziQuotas { by_railway, records: 2 };
        let limits = q.to_limits();
        assert_eq!(limits.get(&("МСК".to_string(), 1)), Some(&25));
        assert_eq!(limits.get(&("МСК".to_string(), 10)), Some(&40));
    }

    /// Группа без «/» трактуется как код дороги целиком; пробелы и регистр нормализуются.
    #[test]
    fn railway_group_normalization() {
        let items = parse_items(
            r#"[{"DMZIRailWayGroup": " мск ", "NormativType": "Ostatok",
                 "DateOfNormativ": "2026-06-10T00:00:00", "Normativ": 5}]"#,
        );
        let q = aggregate(&items, today());
        assert!(q.by_railway.contains_key("МСК"));
    }
}

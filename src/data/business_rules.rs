//! Бизнес-правила логистов при назначении порожних вагонов под погрузку
//! (`data/business_rules.json`, машиночитаемое зеркало `business_rules.txt`).
//!
//! Правила применяются в [`crate::solver::model::classify_pair`] к дугам **погрузки**
//! как жёсткие фильтры и/или надбавки к тарифу. Промывка, отстой и пути клиента
//! правилами не ограничиваются.
//!
//! Дороги сравниваются по коротким кодам: `SupplyNode::railway_to` (RailWayToShort)
//! и `DemandNode::railway_name` (RailWayShortFrom).

use std::collections::HashSet;
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

/// Исключение к правилу инотерриторий: откуда разрешён подсыл на дорогу
/// `demand_railway` и с какой надбавкой к тарифу.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ForeignException {
    /// Дорога погрузки (инотерритория), к которой относится исключение.
    pub demand_railway: String,
    /// Дороги образования порожнего, с которых подсыл разрешён.
    pub from_railways: HashSet<String>,
    /// Надбавка к тарифу, руб./ваг. (0 — без надбавки).
    #[serde(default)]
    pub surcharge_rub: f64,
}

/// Набор бизнес-правил. `Default` — правил 1–2 нет, ограничения на дуги не применяются;
/// проверка ГУ-12 (правило 3) по умолчанию включена.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BusinessRules {
    /// Потолок тарифного расстояния порожнего подсыла под погрузку, км.
    /// `None`/`0` — без потолка. Дальний подсыл (ДВС → центр) не практикуется.
    #[serde(rename = "MaxEmptyRunDistanceKm", deserialize_with = "de_positive_i32")]
    pub max_empty_run_distance_km: Option<i32>,

    /// Правило 1: дороги-инотерритории. Заявка на такой дороге закрывается только
    /// вагонами с той же дороги либо по исключению из [`Self::foreign_exceptions`].
    #[serde(rename = "ForeignRailways")]
    pub foreign_railways: HashSet<String>,

    /// Исключения к правилу 1 (например, КЗХ ← ОКТ/СКВ со штрафом, КЗХ ← Средняя Азия).
    #[serde(rename = "ForeignExceptions")]
    pub foreign_exceptions: Vec<ForeignException>,

    /// Правило 2: дефицитные дороги — порожние с них на другие дороги не забирают.
    #[serde(rename = "DeficitRailways")]
    pub deficit_railways: HashSet<String>,

    /// Исключение к правилу 2: вывоз с дефицитной дороги допустим при тарифном
    /// расстоянии не больше этого порога (км). `0` — вывоз запрещён полностью.
    #[serde(rename = "DeficitExportMaxDistanceKm")]
    pub deficit_export_max_distance_km: i32,

    /// Надбавка к тарифу за вывоз с дефицитной дороги (руб./ваг.), чтобы такой
    /// подсыл оставался исключением, а не нормой при равных тарифах.
    #[serde(rename = "DeficitExportSurchargeRub")]
    pub deficit_export_surcharge_rub: f64,

    /// Правило 3: спрос погрузки на российских дорогах ограничивается согласованными
    /// заявками ГУ-12 ([`crate::data::gu12::apply_gu12_limits`]). `false` — проверка
    /// отключена (спрос АПИ берётся как есть).
    #[serde(rename = "Gu12CheckEnabled")]
    pub gu12_check_enabled: bool,
}

impl Default for BusinessRules {
    fn default() -> Self {
        Self {
            max_empty_run_distance_km: None,
            foreign_railways: HashSet::new(),
            foreign_exceptions: Vec::new(),
            deficit_railways: HashSet::new(),
            deficit_export_max_distance_km: 0,
            deficit_export_surcharge_rub: 0.0,
            gu12_check_enabled: true,
        }
    }
}

/// Число из JSON (число или строка) → `Some(v)` при `v > 0`, иначе `None`.
fn de_positive_i32<'de, D>(d: D) -> Result<Option<i32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Num(i64),
        Str(String),
        Null,
    }
    let v = match Raw::deserialize(d)? {
        Raw::Num(n) => Some(n),
        Raw::Str(s) => s.trim().parse::<i64>().ok(),
        Raw::Null => None,
    };
    Ok(v.filter(|k| *k > 0).map(|k| k.min(i32::MAX as i64) as i32))
}

/// Результат проверки пары дорог `(образование → погрузка)` бизнес-правилами.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RuleOutcome {
    /// Пара допустима; `surcharge_rub` — суммарная надбавка к тарифу (0 — без надбавки).
    Allowed { surcharge_rub: f64 },
    /// Правило 1: подсыл на инотерриторию с этой дороги запрещён.
    ForeignTerritory,
    /// Правило 2: вывоз порожнего с дефицитной дороги (плечо длиннее допустимого).
    DeficitExport,
}

impl BusinessRules {
    /// Загружает правила из JSON-файла.
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("чтение {}", path.display()))?;
        let mut rules: BusinessRules =
            serde_json::from_str(&text).context("разбор business_rules.json")?;
        rules.normalize();
        Ok(rules)
    }

    /// Обрезает пробелы в кодах дорог (сравнение строгое, по коротким кодам).
    fn normalize(&mut self) {
        fn trim_set(s: &HashSet<String>) -> HashSet<String> {
            s.iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
        }
        self.foreign_railways = trim_set(&self.foreign_railways);
        self.deficit_railways = trim_set(&self.deficit_railways);
        for e in &mut self.foreign_exceptions {
            e.demand_railway = e.demand_railway.trim().to_string();
            e.from_railways = trim_set(&e.from_railways);
        }
    }

    /// Есть ли хоть одно активное правило (для логов).
    pub fn is_empty(&self) -> bool {
        self.max_empty_run_distance_km.is_none()
            && self.foreign_railways.is_empty()
            && self.deficit_railways.is_empty()
    }

    /// Проверяет пару дорог для дуги **погрузки**.
    ///
    /// - `supply_railway` — дорога образования порожнего (`SupplyNode::railway_to`);
    /// - `demand_railway` — дорога погрузки (`DemandNode::railway_name`);
    /// - `distance_km` — тарифное расстояние пары станций.
    ///
    /// Порядок: инотерритория → дефицитная дорога. Внутри одной дороги оба правила
    /// не действуют. Потолок расстояния ([`Self::max_empty_run_distance_km`]) здесь
    /// **не** проверяется — он применяется отдельно в `classify_pair`.
    pub fn check_load_pair(
        &self,
        supply_railway: &str,
        demand_railway: &str,
        distance_km: i32,
    ) -> RuleOutcome {
        let s_rw = supply_railway.trim();
        let d_rw = demand_railway.trim();
        let mut surcharge = 0.0_f64;

        if d_rw.is_empty() || s_rw.is_empty() || s_rw == d_rw {
            // Дорога неизвестна либо подсыл внутри одной дороги — правила не действуют.
            return RuleOutcome::Allowed { surcharge_rub: 0.0 };
        }

        // --- Правило 1: инотерритория ---
        if self.foreign_railways.contains(d_rw) {
            let exception = self
                .foreign_exceptions
                .iter()
                .find(|e| e.demand_railway == d_rw && e.from_railways.contains(s_rw));
            match exception {
                Some(e) => surcharge += e.surcharge_rub.max(0.0),
                None => return RuleOutcome::ForeignTerritory,
            }
        }

        // --- Правило 2: вывоз с дефицитной дороги ---
        if self.deficit_railways.contains(s_rw) {
            if distance_km > self.deficit_export_max_distance_km {
                return RuleOutcome::DeficitExport;
            }
            surcharge += self.deficit_export_surcharge_rub.max(0.0);
        }

        RuleOutcome::Allowed { surcharge_rub: surcharge }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> BusinessRules {
        let mut r = BusinessRules {
            max_empty_run_distance_km: Some(5000),
            deficit_export_max_distance_km: 300,
            deficit_export_surcharge_rub: 30_000.0,
            ..Default::default()
        };
        r.foreign_railways = ["КЗХ", "УЗБ", "БЕЛ"].iter().map(|s| s.to_string()).collect();
        r.deficit_railways = ["МСК", "ЮВС"].iter().map(|s| s.to_string()).collect();
        r.foreign_exceptions = vec![
            ForeignException {
                demand_railway: "КЗХ".into(),
                from_railways: ["ОКТ", "СКВ"].iter().map(|s| s.to_string()).collect(),
                surcharge_rub: 50_000.0,
            },
            ForeignException {
                demand_railway: "КЗХ".into(),
                from_railways: ["УЗБ", "КРГ"].iter().map(|s| s.to_string()).collect(),
                surcharge_rub: 0.0,
            },
        ];
        r
    }

    #[test]
    fn same_railway_always_allowed() {
        let r = rules();
        assert_eq!(r.check_load_pair("КЗХ", "КЗХ", 9000), RuleOutcome::Allowed { surcharge_rub: 0.0 });
        assert_eq!(r.check_load_pair("МСК", "МСК", 9000), RuleOutcome::Allowed { surcharge_rub: 0.0 });
    }

    #[test]
    fn russian_to_foreign_forbidden_except_listed() {
        let r = rules();
        // Российская дорога → инотерритория без исключения.
        assert_eq!(r.check_load_pair("ГОР", "УЗБ", 100), RuleOutcome::ForeignTerritory);
        assert_eq!(r.check_load_pair("ГОР", "КЗХ", 100), RuleOutcome::ForeignTerritory);
        // Чужая инотерритория → КЗХ (не в исключениях).
        assert_eq!(r.check_load_pair("БЕЛ", "КЗХ", 100), RuleOutcome::ForeignTerritory);
        // ОКТ/СКВ → КЗХ разрешено с надбавкой.
        assert_eq!(r.check_load_pair("ОКТ", "КЗХ", 3000), RuleOutcome::Allowed { surcharge_rub: 50_000.0 });
        assert_eq!(r.check_load_pair("СКВ", "КЗХ", 3000), RuleOutcome::Allowed { surcharge_rub: 50_000.0 });
        // Средняя Азия → КЗХ без надбавки.
        assert_eq!(r.check_load_pair("УЗБ", "КЗХ", 800), RuleOutcome::Allowed { surcharge_rub: 0.0 });
    }

    #[test]
    fn deficit_export_only_short_haul_with_surcharge() {
        let r = rules();
        assert_eq!(r.check_load_pair("МСК", "ГОР", 301), RuleOutcome::DeficitExport);
        assert_eq!(r.check_load_pair("МСК", "ГОР", 300), RuleOutcome::Allowed { surcharge_rub: 30_000.0 });
        // Дефицитная → дефицитная: то же правило.
        assert_eq!(r.check_load_pair("МСК", "ЮВС", 250), RuleOutcome::Allowed { surcharge_rub: 30_000.0 });
        assert_eq!(r.check_load_pair("МСК", "ЮВС", 900), RuleOutcome::DeficitExport);
        // Недефицитная → любая: без ограничений.
        assert_eq!(r.check_load_pair("ГОР", "МСК", 2000), RuleOutcome::Allowed { surcharge_rub: 0.0 });
    }

    #[test]
    fn unknown_railway_is_not_restricted() {
        let r = rules();
        assert_eq!(r.check_load_pair("", "КЗХ", 100), RuleOutcome::Allowed { surcharge_rub: 0.0 });
        assert_eq!(r.check_load_pair("МСК", "", 5000), RuleOutcome::Allowed { surcharge_rub: 0.0 });
    }

    #[test]
    fn default_rules_restrict_nothing() {
        let r = BusinessRules::default();
        assert!(r.is_empty());
        assert_eq!(r.check_load_pair("ГОР", "КЗХ", 9000), RuleOutcome::Allowed { surcharge_rub: 0.0 });
    }

    /// Боевой справочник `data/business_rules.json` разбирается и отражает business_rules.txt.
    #[test]
    fn repo_business_rules_json_is_valid() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/business_rules.json");
        let r = BusinessRules::load(&path).unwrap();
        // Конкретное значение потолка — настройка логистов, тест проверяет только наличие.
        assert!(r.max_empty_run_distance_km.is_some_and(|km| km >= 1000), "потолок подсыла задан и разумен");
        for rw in ["КЗХ", "КРГ", "ТДЖ", "УЗБ", "ТРК", "АЗР", "ГРЗ", "ЮКЖ", "БЕЛ", "ЛАТ", "ЭСТ", "ЛИТ"] {
            assert!(r.foreign_railways.contains(rw), "нет инотерритории {rw}");
        }
        for rw in ["ЮВС", "МСК", "КБШ", "ПРВ", "ЮУР", "ЗСБ"] {
            assert!(r.deficit_railways.contains(rw), "нет дефицитной дороги {rw}");
        }
        // Российская дорога → инотерритория запрещена; КЗХ ← ОКТ/СКВ со штрафом; ← Средняя Азия свободно.
        assert_eq!(r.check_load_pair("ГОР", "УЗБ", 500), RuleOutcome::ForeignTerritory);
        assert_eq!(r.check_load_pair("ГОР", "КЗХ", 500), RuleOutcome::ForeignTerritory);
        assert!(matches!(r.check_load_pair("ОКТ", "КЗХ", 3000), RuleOutcome::Allowed { surcharge_rub } if surcharge_rub > 0.0));
        assert!(matches!(r.check_load_pair("СКВ", "КЗХ", 3000), RuleOutcome::Allowed { surcharge_rub } if surcharge_rub > 0.0));
        assert_eq!(r.check_load_pair("УЗБ", "КЗХ", 800), RuleOutcome::Allowed { surcharge_rub: 0.0 });
        // Вывоз с дефицитной дороги — только короткое плечо.
        assert_eq!(r.check_load_pair("ЮВС", "СКВ", 1000), RuleOutcome::DeficitExport);
        assert!(matches!(r.check_load_pair("ЮВС", "СКВ", 200), RuleOutcome::Allowed { surcharge_rub } if surcharge_rub > 0.0));
        // Правило 3 включено.
        assert!(r.gu12_check_enabled);
    }

    #[test]
    fn gu12_check_defaults_to_enabled_and_can_be_disabled() {
        assert!(BusinessRules::default().gu12_check_enabled);
        let r: BusinessRules = serde_json::from_str("{}").unwrap();
        assert!(r.gu12_check_enabled);
        let r: BusinessRules = serde_json::from_str(r#"{"Gu12CheckEnabled": false}"#).unwrap();
        assert!(!r.gu12_check_enabled);
    }

    #[test]
    fn loads_json_with_comments_and_string_distance() {
        let dir = std::env::temp_dir().join(format!("railoptim_rules_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("business_rules.json");
        std::fs::write(
            &path,
            r#"{
                "_comment": "x",
                "MaxEmptyRunDistanceKm": " 4500 ",
                "ForeignRailways": [" КЗХ ", "УЗБ"],
                "ForeignExceptions": [{"demand_railway": "КЗХ", "from_railways": ["ОКТ "], "surcharge_rub": 1000}],
                "DeficitRailways": ["МСК"],
                "DeficitExportMaxDistanceKm": 200,
                "DeficitExportSurchargeRub": 5000
            }"#,
        )
        .unwrap();
        let r = BusinessRules::load(&path).unwrap();
        assert_eq!(r.max_empty_run_distance_km, Some(4500));
        assert!(r.foreign_railways.contains("КЗХ"));
        assert_eq!(r.check_load_pair("ОКТ", "КЗХ", 100), RuleOutcome::Allowed { surcharge_rub: 1000.0 });
        assert_eq!(r.check_load_pair("МСК", "ГОР", 201), RuleOutcome::DeficitExport);

        // Пустой объект — правил нет, потолка нет.
        std::fs::write(&path, "{}").unwrap();
        let r = BusinessRules::load(&path).unwrap();
        assert!(r.is_empty());
        assert_eq!(r.max_empty_run_distance_km, None);

        // 0 → потолок отключён.
        std::fs::write(&path, r#"{"MaxEmptyRunDistanceKm": 0}"#).unwrap();
        assert_eq!(BusinessRules::load(&path).unwrap().max_empty_run_distance_km, None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

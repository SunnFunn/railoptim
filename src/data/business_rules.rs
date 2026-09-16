//! Бизнес-правила логистов при назначении порожних вагонов под погрузку
//! (`data/business_rules.json`, машиночитаемое зеркало `business_rules.txt`).
//!
//! Правила применяются в [`crate::solver::model::classify_pair`] к дугам **погрузки**
//! как жёсткие фильтры и/или надбавки (правило 6 — поощрение, правило 8 — капризная
//! дорога при профиците) к тарифу и при группировке предложения (правило 7 —
//! горизонт вывода в ремонт). Правило 8 также снимает «грязность» с вагонов
//! иномойки: они не едут в промывку. Отстой и пути клиента правилами не
//! ограничиваются; правило 6 задаёт стоимость промывочного маршрута, с которой
//! сравнивается прямая погрузка.
//!
//! Дороги сравниваются по коротким кодам: `SupplyNode::railway_to` (RailWayToShort)
//! и `DemandNode::railway_name` (RailWayShortFrom).

use std::collections::HashSet;
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

use crate::node::{DemandNode, DemandPurpose, SupplyNode};

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
    /// Тот же список — территория вне России для ГУ-12 (правило 3) и горизонт
    /// ремонта на инотерритории (правило 7).
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
    /// отключена (спрос АПИ берётся как есть). Территория России — дороги **не** из
    /// [`Self::foreign_railways`]: пустой список → ГУ-12 не выполняется
    /// ([`Self::gu12_ready`]), иначе нельзя ограничить проверку Россией.
    #[serde(rename = "Gu12CheckEnabled")]
    pub gu12_check_enabled: bool,

    /// Правило 5: конвенции РЖД из HASH `telegrams_db` (`conv-redis`).
    /// Действующие запреты идут в `classify_pair` (погрузка/промывка), отстой и ремонт;
    /// `false` — к Redis не ходим, индекс пустой.
    #[serde(rename = "ConventionCheckEnabled")]
    pub convention_check_enabled: bool,

    /// Правило 4 (жёсткая часть): станция погрузки **закрыта** для подсыла во все
    /// периоды, если вагонов на станции (сумма `CarsOnStation` из АПИ спроса по
    /// грузоотправителям) не меньше `K_hard × мощность погрузки в сутки`
    /// (`station_load_capacity` из `data/load_stations.json`).
    /// Большая очередь — признак остановившейся погрузки
    /// (отказ принимать груз), экстраполировать её рассасывание нельзя.
    /// `None`/`0` — правило 4 целиком отключено. Станции с неизвестной мощностью
    /// (0 или нет в справочнике) не проверяются.
    #[serde(rename = "StationBacklogHardDays", deserialize_with = "de_positive_i32")]
    pub station_backlog_hard_days: Option<i32>,

    /// Правило 4 (мягкая часть): допустимая очередь на станции в **сутках работы**
    /// на момент прибытия вагона. Пока очередь длиннее `K_soft × мощность`, вагон
    /// стоит; ожидаемые сутки погрузки сдвигаются на срок рассасывания очереди
    /// (`t* = ceil((Q − K_soft·C) / C)`), и уже они проверяются окном периода спроса.
    /// Должно быть меньше `StationBacklogHardDays`, иначе мягкая часть не работает.
    #[serde(rename = "StationBacklogSoftDays")]
    pub station_backlog_soft_days: i32,

    /// Штраф к тарифу (руб./ваг. за сутки) за каждые сутки ожидания погрузки на
    /// станции из-за очереди (мягкая часть правила 4). Вагон стоит на путях клиента,
    /// поэтому при близких тарифах предпочтителен тот, что приедет к моменту, когда
    /// очередь рассосётся.
    #[serde(rename = "StationBacklogWaitPenaltyRubPerDay")]
    pub station_backlog_wait_penalty_rub_per_day: f64,

    /// Правило 6: средняя стоимость самой промывки вагона (руб./ваг.). Вместе с
    /// [`Self::empty_run_after_wash_cost_rub`] образует надбавку к тарифу до станции
    /// промывки ([`Self::wash_path_surcharge_rub`]) — модельную стоимость
    /// «промывочного маршрута» (доехать до промывки, промыться, доехать чистым
    /// под погрузку), с которой сравнивается прямая погрузка аналогичного груза.
    #[serde(rename = "WashProcedureCostRub")]
    pub wash_procedure_cost_rub: f64,

    /// Правило 6: средний тариф порожнего пробега от станции промывки до
    /// следующей погрузки (руб./ваг.). Сама погрузка после промывки в прогон не
    /// попадает (вагон вернётся в предложение следующих суток чистым), поэтому
    /// этот пробег учитывается средней надбавкой.
    #[serde(rename = "EmptyRunAfterWashCostRub")]
    pub empty_run_after_wash_cost_rub: f64,

    /// Правило 6 (потолок дальности): дуга «грязный → погрузка того же ЕТСНГ»
    /// строится, только если её модельная стоимость не больше
    /// `k × min(промывочный маршрут со станции образования)`, где промывочный
    /// маршрут = тариф до промывки + [`Self::wash_path_surcharge_rub`].
    /// `1.0` — прямая погрузка не дороже, чем промыться и подослать чистым;
    /// `> 1` — допускается более дальняя прямая погрузка; `≤ 0` — потолок отключён
    /// (остаётся только `MaxEmptyRunDistanceKm`). Без промывочного маршрута
    /// (нет wash-тарифа) потолок не применяется — прямая погрузка единственный шанс.
    #[serde(rename = "DirtySameCargoMaxCostRatioToWash")]
    pub dirty_same_cargo_max_cost_ratio_to_wash: f64,

    /// Правило 6 (поощрение): доля `p ∈ [0, 1]` отложенной промывки, снимаемая со
    /// стоимости дуги «грязный → погрузка того же ЕТСНГ». Грязный вагон, не
    /// погруженный сегодня под свой груз, завтра с вероятностью `p` поедет в
    /// промывку: тариф до ближайшей промывки + сама промывка
    /// (`wash_procedure_cost_rub`); чистый вагон такой «повинности» не несёт.
    /// Поощрение = `p × (тариф до ближайшей промывки + промывка)`; без wash-тарифа —
    /// `p × wash_path_surcharge_rub`. `0` — без поощрения (грязный и чистый вагон
    /// конкурируют за заявку по одному тарифу); `1` — промывка неизбежна.
    #[serde(rename = "DirtySameCargoRewardShare")]
    pub dirty_same_cargo_reward_share: f64,

    /// Правило 7: горизонт вывода в ремонт на российских дорогах (сут.). Вагон с
    /// `CarNextRepairDays` строго меньше этого порога идёт в ремонт (`NeedsRepair`),
    /// а не в оптимизацию. `IsCarRepair = true` выводит независимо от срока.
    /// `0` — проверка по дням отключена (остаётся только флаг АПИ).
    #[serde(rename = "RepairDaysThreshold")]
    pub repair_days_threshold: i32,

    /// Правило 7: тот же горизонт на инотерритории ([`Self::foreign_railways`],
    /// дорога образования `SupplyNode::railway_to`). Длиннее российского: вагон
    /// нужно успеть вывезти с инотерритории до ремонта. `0` — как российский порог.
    #[serde(rename = "RepairDaysThresholdForeign")]
    pub repair_days_threshold_foreign: i32,

    /// Правило 8: дороги образования, на которых клиент моет вагон сам (иномойка).
    /// Такие вагоны не считаются грязными для правила 6 и не едут в российскую промывку.
    #[serde(rename = "ForeignWashedRoads")]
    pub foreign_washed_roads: HashSet<String>,

    /// Правило 8: капризные дороги погрузки. При профиците порожних подсыл
    /// иномойки сюда дороже на [`Self::foreign_washed_picky_surcharge_rub`].
    #[serde(rename = "ForeignWashedPickyRailways")]
    pub foreign_washed_picky_railways: HashSet<String>,

    /// Правило 8: надбавка (руб./ваг.) к тарифу «иномойка → капризная дорога»
    /// при профиците предложения над спросом погрузки. `0` — капризная часть выключена.
    #[serde(rename = "ForeignWashedPickySurchargeRub")]
    pub foreign_washed_picky_surcharge_rub: f64,

    /// Правило 8: порог профицита — предложение должно превышать спрос погрузки
    /// в это число раз (`> ratio × спрос`), чтобы капризная надбавка включилась.
    /// Запас над 1.0 защищает от переключения надбавки из-за разницы в несколько
    /// вагонов. Меньше 1.0 поднимается до 1.0 (иначе «профицит» при дефиците).
    #[serde(rename = "ForeignWashedPickySurplusRatio")]
    pub foreign_washed_picky_surplus_ratio: f64,

    /// Аддитивная поправка к модельной стоимости дуги **погрузки** для вагонов
    /// периода 1: нейтральная дальность `d₀` (км). Ближе — поправка отрицательна
    /// (бонус сегодняшнему вагону на короткое плечо), дальше — положительна (дальний
    /// подсыл уходит вагонам периода 10). В отчёт не попадает: Excel/API пишут
    /// исходный тариф (`TaskArc::tariff_cost`).
    #[serde(rename = "P1DistanceNeutralKm")]
    pub p1_distance_neutral_km: i32,

    /// Ставка поправки, руб./ваг. за каждый км разницы `d − d₀`.
    /// `0` — поправка выключена.
    #[serde(rename = "P1DistanceRubPerKm")]
    pub p1_distance_rub_per_km: f64,

    /// Потолок модуля поправки, руб./ваг. `0` — без потолка.
    #[serde(rename = "P1DistanceCapRub")]
    pub p1_distance_cap_rub: f64,
}

/// Значение [`BusinessRules::foreign_washed_picky_surplus_ratio`] по умолчанию.
pub const DEFAULT_FOREIGN_WASHED_PICKY_SURPLUS_RATIO: f64 = 1.2;

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
            convention_check_enabled: true,
            station_backlog_hard_days: None,
            station_backlog_soft_days: 1,
            station_backlog_wait_penalty_rub_per_day: 0.0,
            wash_procedure_cost_rub: 10_000.0,
            empty_run_after_wash_cost_rub: 40_000.0,
            dirty_same_cargo_max_cost_ratio_to_wash: 1.0,
            dirty_same_cargo_reward_share: 0.0,
            repair_days_threshold: 15,
            repair_days_threshold_foreign: 45,
            foreign_washed_roads: HashSet::new(),
            foreign_washed_picky_railways: HashSet::new(),
            foreign_washed_picky_surcharge_rub: 0.0,
            foreign_washed_picky_surplus_ratio: DEFAULT_FOREIGN_WASHED_PICKY_SURPLUS_RATIO,
            p1_distance_neutral_km: 0,
            p1_distance_rub_per_km: 0.0,
            p1_distance_cap_rub: 0.0,
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

/// Результат проверки дуги «грязный вагон → погрузка того же ЕТСНГ» правилом 6.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DirtyLoadOutcome {
    /// Дуга допустима; `reward_rub` — поощрение (≥ 0), которое снимается со стоимости
    /// дуги, чтобы грязный вагон выигрывал заявку у чистого при близких тарифах.
    Allowed { reward_rub: f64 },
    /// Прямая погрузка дороже промывочного маршрута с учётом коэффициента —
    /// вагон должен идти в промывку, дуга не строится.
    PreferWash,
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
        self.foreign_washed_roads = trim_set(&self.foreign_washed_roads);
        self.foreign_washed_picky_railways = trim_set(&self.foreign_washed_picky_railways);
        for e in &mut self.foreign_exceptions {
            e.demand_railway = e.demand_railway.trim().to_string();
            e.from_railways = trim_set(&e.from_railways);
        }
        // Правило 4: мягкий порог не отрицательный и строго меньше жёсткого,
        // иначе t* всегда 0 и мягкая часть не действует.
        self.station_backlog_soft_days = self.station_backlog_soft_days.max(0);
        if let Some(hard) = self.station_backlog_hard_days.filter(|h| self.station_backlog_soft_days >= *h) {
            eprintln!(
                "  [!] business_rules.json: StationBacklogSoftDays ({}) >= StationBacklogHardDays ({hard}) — мягкая часть правила 4 не действует",
                self.station_backlog_soft_days,
            );
        }
        self.station_backlog_wait_penalty_rub_per_day =
            self.station_backlog_wait_penalty_rub_per_day.max(0.0);
        // Правило 6: стоимости не отрицательные, доля поощрения в [0, 1].
        self.wash_procedure_cost_rub = self.wash_procedure_cost_rub.max(0.0);
        self.empty_run_after_wash_cost_rub = self.empty_run_after_wash_cost_rub.max(0.0);
        if self.dirty_same_cargo_reward_share > 1.0 {
            eprintln!(
                "  [!] business_rules.json: DirtySameCargoRewardShare ({}) > 1 — поощрение не может превышать стоимость отложенной промывки, берётся 1",
                self.dirty_same_cargo_reward_share,
            );
        }
        self.dirty_same_cargo_reward_share = self.dirty_same_cargo_reward_share.clamp(0.0, 1.0);
        // Правило 7: пороги не отрицательные; инотерриториальный не короче российского
        // (иначе вывоз с инотерритории успевали бы меньше, чем ремонт в России).
        self.repair_days_threshold = self.repair_days_threshold.max(0);
        self.repair_days_threshold_foreign = self.repair_days_threshold_foreign.max(0);
        if self.repair_days_threshold_foreign > 0
            && self.repair_days_threshold_foreign < self.repair_days_threshold
        {
            eprintln!(
                "  [!] business_rules.json: RepairDaysThresholdForeign ({}) < RepairDaysThreshold ({}) — для инотерритории берётся российский порог",
                self.repair_days_threshold_foreign, self.repair_days_threshold,
            );
            self.repair_days_threshold_foreign = self.repair_days_threshold;
        }
        self.foreign_washed_picky_surcharge_rub = self.foreign_washed_picky_surcharge_rub.max(0.0);
        // Правило 8: порог профицита не ниже 1 — иначе надбавка включалась бы при дефиците.
        if !self.foreign_washed_picky_surplus_ratio.is_finite() {
            self.foreign_washed_picky_surplus_ratio = DEFAULT_FOREIGN_WASHED_PICKY_SURPLUS_RATIO;
        }
        if self.foreign_washed_picky_surplus_ratio < 1.0 {
            eprintln!(
                "  [!] business_rules.json: ForeignWashedPickySurplusRatio ({}) < 1 — профицит не может наступать при дефиците, берётся 1",
                self.foreign_washed_picky_surplus_ratio,
            );
            self.foreign_washed_picky_surplus_ratio = 1.0;
        }
        // Поправка периода 1 по расстоянию: мусор → выкл., отрицательная ставка —
        // предупреждение (дальние подсылы периода 1 станут дешевле ближних).
        if !self.p1_distance_rub_per_km.is_finite() {
            self.p1_distance_rub_per_km = 0.0;
        }
        if !self.p1_distance_cap_rub.is_finite() || self.p1_distance_cap_rub < 0.0 {
            self.p1_distance_cap_rub = 0.0;
        }
        self.p1_distance_neutral_km = self.p1_distance_neutral_km.max(0);
        if self.p1_distance_rub_per_km < 0.0 {
            eprintln!(
                "  [!] business_rules.json: P1DistanceRubPerKm ({}) < 0 — дальние подсылы периода 1 станут дешевле ближних",
                self.p1_distance_rub_per_km,
            );
        }
    }

    /// Правило 4 включено (задан жёсткий порог `StationBacklogHardDays`).
    pub fn station_backlog_enabled(&self) -> bool {
        self.station_backlog_hard_days.is_some()
    }

    /// Правило 3 можно применить: флаг включён **и** список инотерриторий не пуст.
    ///
    /// Пустой [`Self::foreign_railways`] означает, что проверку нельзя ограничить
    /// территорией России (в том числе при непрочитанном `business_rules.json`:
    /// `gu12_check_enabled` по умолчанию `true`, список пуст) — тогда ГУ-12
    /// не выполняется, иначе срежет спрос на инотерриториях.
    pub fn gu12_ready(&self) -> bool {
        self.gu12_check_enabled && !self.foreign_railways.is_empty()
    }

    /// Правило 6: надбавка к тарифу до станции промывки (руб./ваг.) — промывка +
    /// средний порожний пробег после неё до погрузки. Прибавляется к wash-тарифам
    /// при их загрузке, чтобы Wash-дуга стоила как весь промывочный маршрут.
    pub fn wash_path_surcharge_rub(&self) -> f64 {
        self.wash_procedure_cost_rub + self.empty_run_after_wash_cost_rub
    }

    /// Правило 6: потолок дальности прямой погрузки грязного вагона включён.
    pub fn dirty_same_cargo_cap_enabled(&self) -> bool {
        self.dirty_same_cargo_max_cost_ratio_to_wash > 0.0
    }

    /// Правило 7: горизонт вывода в ремонт (сут.) для дороги образования вагона.
    ///
    /// `supply_railway` — короткий код `RailWayToShort` (где вагон сейчас). На дороге
    /// из [`Self::foreign_railways`] — [`Self::repair_days_threshold_foreign`] (если > 0),
    /// иначе российский [`Self::repair_days_threshold`]. `0` — проверка по дням выключена.
    pub fn repair_days_threshold_for(&self, supply_railway: &str) -> i32 {
        let rw = supply_railway.trim();
        if !rw.is_empty()
            && self.foreign_railways.contains(rw)
            && self.repair_days_threshold_foreign > 0
        {
            self.repair_days_threshold_foreign
        } else {
            self.repair_days_threshold
        }
    }

    /// Правило 7: вагон идёт в ремонт, а не в оптимизацию.
    ///
    /// `IsCarRepair` — безусловно. Иначе `CarNextRepairDays` строго меньше порога
    /// для дороги образования ([`Self::repair_days_threshold_for`]). Нет срока — не
    /// выводим (как прежнее `unwrap_or(false)`).
    pub fn wagon_needs_repair(
        &self,
        is_car_repair: bool,
        days_to_repair: Option<f64>,
        supply_railway: &str,
    ) -> bool {
        if is_car_repair {
            return true;
        }
        let threshold = self.repair_days_threshold_for(supply_railway);
        if threshold <= 0 {
            return false;
        }
        days_to_repair.map(|d| d < threshold as f64).unwrap_or(false)
    }

    /// Правило 8: вагон образовался на дороге иномойки — клиент уже обязан вернуть его чистым.
    pub fn is_foreign_washed(&self, supply_railway: &str) -> bool {
        let rw = supply_railway.trim();
        !rw.is_empty() && self.foreign_washed_roads.contains(rw)
    }

    /// Поправка периода 1 по расстоянию включена (ставка ненулевая).
    pub fn p1_distance_adjust_enabled(&self) -> bool {
        self.p1_distance_rub_per_km != 0.0
    }

    /// Выключить поправку периода 1 по расстоянию (ставка → 0). Используется в режиме
    /// `--day1` (`INCLUDE_PERIOD10=off`): без вагонов периода 10 конкурировать не с кем,
    /// поправка только искажала бы выбор между заявками для одного и того же вагона.
    pub fn disable_p1_distance_adjust(&mut self) {
        self.p1_distance_rub_per_km = 0.0;
    }

    /// Аддитивная поправка (руб./ваг.) к модельной стоимости по расстоянию:
    /// `R × (d − d₀)`, по модулю не больше [`Self::p1_distance_cap_rub`] (если > 0).
    /// Отрицательна ближе `d₀`, положительна дальше. Выключено → `0`.
    pub fn p1_distance_adjust_rub(&self, distance_km: i32) -> f64 {
        if !self.p1_distance_adjust_enabled() {
            return 0.0;
        }
        let delta_km = f64::from(distance_km.max(0) - self.p1_distance_neutral_km);
        let raw = self.p1_distance_rub_per_km * delta_km;
        if self.p1_distance_cap_rub > 0.0 {
            raw.clamp(-self.p1_distance_cap_rub, self.p1_distance_cap_rub)
        } else {
            raw
        }
    }

    /// Поправка к модельной стоимости дуги: только период 1 и `DemandPurpose::Load`;
    /// промывка и период 10 — `0`. К отчётному тарифу не относится.
    pub fn p1_load_distance_adjust_rub(
        &self,
        supply_period: u8,
        purpose: DemandPurpose,
        distance_km: i32,
    ) -> f64 {
        if supply_period != 1 || purpose != DemandPurpose::Load {
            0.0
        } else {
            self.p1_distance_adjust_rub(distance_km)
        }
    }

    /// Правило 8: на рынке профицит порожних — суммарное предложение больше
    /// `ForeignWashedPickySurplusRatio × спрос погрузки` — и капризная надбавка включена.
    pub fn foreign_washed_picky_active(&self, total_supply: i32, total_load_demand: i32) -> bool {
        (total_supply as f64) > self.foreign_washed_picky_surplus_ratio * (total_load_demand as f64)
            && self.foreign_washed_picky_surcharge_rub > 0.0
            && !self.foreign_washed_roads.is_empty()
            && !self.foreign_washed_picky_railways.is_empty()
    }

    /// Правило 8: профицит по узлам текущего прогона — свободное предложение
    /// (`opt_supply`, периоды 1 и 10 вместе) против спроса **погрузки** (`Load`,
    /// узлы промывки не считаются). Единая точка расчёта для построения дуг,
    /// диагностики незакрытого спроса и лога.
    pub fn market_surplus(&self, supply: &[SupplyNode], demand: &[DemandNode]) -> bool {
        let total_supply: i32 = supply.iter().map(|s| s.car_count).sum();
        let total_load: i32 = demand
            .iter()
            .filter(|d| d.purpose == DemandPurpose::Load)
            .map(|d| d.car_count)
            .sum();
        self.foreign_washed_picky_active(total_supply, total_load)
    }

    /// Правило 8: надбавка к дуге погрузки «иномойка → капризная дорога».
    ///
    /// `market_surplus` — [`Self::foreign_washed_picky_active`] для текущего прогона.
    /// Без профицита, пустых списков или нулевой надбавки возвращает 0: капризная
    /// дорога при дефиците берёт любые вагоны.
    pub fn foreign_washed_picky_surcharge(
        &self,
        supply_railway: &str,
        demand_railway: &str,
        market_surplus: bool,
    ) -> f64 {
        if !market_surplus || self.foreign_washed_picky_surcharge_rub <= 0.0 {
            return 0.0;
        }
        let s_rw = supply_railway.trim();
        let d_rw = demand_railway.trim();
        if s_rw.is_empty() || d_rw.is_empty() {
            return 0.0;
        }
        if self.foreign_washed_roads.contains(s_rw)
            && self.foreign_washed_picky_railways.contains(d_rw)
        {
            self.foreign_washed_picky_surcharge_rub
        } else {
            0.0
        }
    }

    /// Правило 6: проверка дуги «грязный вагон → погрузка того же ЕТСНГ».
    ///
    /// - `direct_cost` — полная модельная стоимость прямой погрузки (тариф + штрафы за
    ///   срок и очередь + надбавки правил дорог) **до** поощрения;
    /// - `wash_route_min_cost` — минимальная стоимость промывочного маршрута со станции
    ///   образования (тариф до промывки + [`Self::wash_path_surcharge_rub`]), `None` —
    ///   промывка для вагона недоступна (нет wash-тарифа).
    ///
    /// Потолок: `direct_cost > k × wash_route_min_cost` → [`DirtyLoadOutcome::PreferWash`]
    /// (`k` = [`Self::dirty_same_cargo_max_cost_ratio_to_wash`], `≤ 0` — потолка нет).
    /// Поощрение: `p × (тариф до ближайшей промывки + промывка)` — ожидаемая стоимость
    /// отложенной промывки, которой чистый вагон-конкурент не несёт; без промывочного
    /// маршрута тариф до промывки неизвестен, берётся средняя надбавка целиком.
    /// Поощрение считается **от того же порога**, что и потолок: чем дороже вагону
    /// промывка, тем больше он «стоит» под своим грузом и тем дальше за ним можно ехать.
    pub fn check_dirty_same_cargo(
        &self,
        direct_cost: f64,
        wash_route_min_cost: Option<f64>,
    ) -> DirtyLoadOutcome {
        let surcharge = self.wash_path_surcharge_rub();
        let deferred_wash_cost = match wash_route_min_cost {
            Some(wash_cost) => {
                if self.dirty_same_cargo_cap_enabled()
                    && direct_cost > self.dirty_same_cargo_max_cost_ratio_to_wash * wash_cost
                {
                    return DirtyLoadOutcome::PreferWash;
                }
                // wash_cost уже содержит надбавку; отложенная промывка = доехать до
                // ближайшей промывки + сама промывка (порожний пробег после неё будет
                // и у чистого вагона, поэтому в разницу не входит).
                (wash_cost - surcharge).max(0.0) + self.wash_procedure_cost_rub
            }
            None => surcharge,
        };
        DirtyLoadOutcome::Allowed {
            reward_rub: (self.dirty_same_cargo_reward_share * deferred_wash_cost).max(0.0),
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

    /// Все дороги колеи 1520 вне РФ по кодам NSI — ожидаемое содержимое `ForeignRailways`.
    pub(crate) const FOREIGN_1520: &[&str] = &[
        "КЗХ", "КРГ", "ТДЖ", "УЗБ", "ТРК", "АЗР", "ГРЗ", "ЮКЖ", "БЕЛ", "ЛАТ", "ЭСТ", "ЛИТ",
        "МЛД", "МНГ", "ЛЬВ", "ЮЗП", "ЮЖН", "ДОН", "ОДС", "ПДН", "FIN",
    ];

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
        for rw in FOREIGN_1520 {
            assert!(r.foreign_railways.contains(*rw), "нет инотерритории {rw}");
        }
        assert!(!r.foreign_railways.contains("МСК"));
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
        // Правило 3 включено; список инотерриторий не пуст — ГУ-12 можно ограничить Россией.
        assert!(r.gu12_check_enabled);
        assert!(r.gu12_ready());
        assert!(r.convention_check_enabled);
        // Правило 4: жёсткий порог задан, мягкий строго меньше.
        let hard = r.station_backlog_hard_days.expect("StationBacklogHardDays задан");
        assert!(hard >= 1);
        assert!(r.station_backlog_soft_days < hard);
        assert!(r.station_backlog_wait_penalty_rub_per_day >= 0.0);
        // Правило 6: стоимость промывочного маршрута задана, потолок и поощрение в разумных пределах.
        assert!(r.wash_procedure_cost_rub > 0.0);
        assert!(r.empty_run_after_wash_cost_rub > 0.0);
        assert!(r.dirty_same_cargo_cap_enabled(), "потолок дальности прямой погрузки включён");
        assert!((0.0..=1.0).contains(&r.dirty_same_cargo_reward_share));
        // Правило 7: российский порог 15, инотерритория 45 (вывоз до ремонта).
        assert_eq!(r.repair_days_threshold, 15);
        assert_eq!(r.repair_days_threshold_foreign, 45);
        assert!(!r.wagon_needs_repair(false, Some(15.0), "МСК"));
        assert!(r.wagon_needs_repair(false, Some(14.0), "МСК"));
        assert!(r.wagon_needs_repair(false, Some(20.0), "КЗХ"));
        assert!(!r.wagon_needs_repair(false, Some(45.0), "КЗХ"));
        // Правило 8: иномойка — те же инотерритории, что правило 1; капризная МСК при профиците.
        assert_eq!(
            r.foreign_washed_roads, r.foreign_railways,
            "ForeignWashedRoads должен совпадать с ForeignRailways"
        );
        assert!(r.is_foreign_washed("КЗХ"));
        assert!(!r.is_foreign_washed("МСК"));
        assert!(r.foreign_washed_picky_railways.contains("МСК"));
        assert!(r.foreign_washed_picky_surcharge_rub > 0.0);
        assert!(r.foreign_washed_picky_surplus_ratio >= 1.0);
        // Профицит с запасом: 121 > 1.2 × 100, 120 — нет.
        assert!(r.foreign_washed_picky_active(121, 100));
        assert!(!r.foreign_washed_picky_active(120, 100));
        assert_eq!(
            r.foreign_washed_picky_surcharge("КЗХ", "МСК", true),
            r.foreign_washed_picky_surcharge_rub,
        );
        assert_eq!(r.foreign_washed_picky_surcharge("КЗХ", "МСК", false), 0.0);
        assert_eq!(r.foreign_washed_picky_surcharge("КЗХ", "ЮВС", true), 0.0);
        assert_eq!(r.foreign_washed_picky_surcharge("СКВ", "МСК", true), 0.0);
        // Поправка периода 1 по расстоянию — настройка теста гипотезы; проверяем только
        // корректность (конечные значения, неотрицательные d₀ и потолок), не включённость.
        assert!(r.p1_distance_rub_per_km.is_finite());
        assert!(r.p1_distance_neutral_km >= 0);
        assert!(r.p1_distance_cap_rub >= 0.0);
        if r.p1_distance_adjust_enabled() {
            assert!(r.p1_distance_rub_per_km > 0.0, "ставка < 0 переворачивает смысл поправки");
            assert!(r.p1_distance_adjust_rub(0) <= 0.0);
            assert!(r.p1_distance_adjust_rub(r.p1_distance_neutral_km).abs() < 1e-9);
        }
    }

    // -----------------------------------------------------------------------
    // Правило 6: грязный вагон под аналогичный груз
    // -----------------------------------------------------------------------

    /// Значения по умолчанию воспроизводят прежние константы: надбавка 10 000 + 40 000,
    /// потолок «не дороже промывочного маршрута», поощрения нет.
    #[test]
    fn dirty_same_cargo_defaults_match_legacy_behaviour() {
        let r = BusinessRules::default();
        assert_eq!(r.wash_path_surcharge_rub(), 50_000.0);
        assert!(r.dirty_same_cargo_cap_enabled());
        // Промывочный маршрут 70 000 (тариф 20 000 + надбавка): 70 000 ещё допустимо, 70 001 — нет.
        assert_eq!(r.check_dirty_same_cargo(70_000.0, Some(70_000.0)), DirtyLoadOutcome::Allowed { reward_rub: 0.0 });
        assert_eq!(r.check_dirty_same_cargo(70_001.0, Some(70_000.0)), DirtyLoadOutcome::PreferWash);
        // Промывки нет — потолка нет, поощрения нет.
        assert_eq!(r.check_dirty_same_cargo(900_000.0, None), DirtyLoadOutcome::Allowed { reward_rub: 0.0 });
        let r: BusinessRules = serde_json::from_str("{}").unwrap();
        assert_eq!(r.wash_path_surcharge_rub(), 50_000.0);
        assert_eq!(r.dirty_same_cargo_reward_share, 0.0);
    }

    /// Поощрение = p × (тариф до ближайшей промывки + промывка): зависит от того,
    /// насколько промывка далека от станции образования, а не от дальности погрузки.
    #[test]
    fn dirty_same_cargo_reward_is_share_of_deferred_wash() {
        let r = BusinessRules {
            dirty_same_cargo_reward_share: 0.5,
            ..Default::default()
        };
        // Промывка в 20 000 от станции: маршрут 70 000; отложенная промывка 20 000 + 10 000.
        assert_eq!(r.check_dirty_same_cargo(30_000.0, Some(70_000.0)), DirtyLoadOutcome::Allowed { reward_rub: 15_000.0 });
        // Поощрение одинаково для ближней и дальней погрузки с той же станции.
        assert_eq!(r.check_dirty_same_cargo(69_000.0, Some(70_000.0)), DirtyLoadOutcome::Allowed { reward_rub: 15_000.0 });
        // Промывка рядом (тариф 2 000): поощрение почти только за саму промывку.
        assert_eq!(r.check_dirty_same_cargo(30_000.0, Some(52_000.0)), DirtyLoadOutcome::Allowed { reward_rub: 6_000.0 });
        // Промывка далеко (тариф 100 000): вагон дорого мыть — дорого стоит под своим грузом.
        assert_eq!(r.check_dirty_same_cargo(30_000.0, Some(150_000.0)), DirtyLoadOutcome::Allowed { reward_rub: 55_000.0 });
        // Промывка недоступна: тариф до неё неизвестен — берётся средняя надбавка целиком.
        assert_eq!(r.check_dirty_same_cargo(30_000.0, None), DirtyLoadOutcome::Allowed { reward_rub: 25_000.0 });
        // Потолок проверяется по стоимости ДО поощрения.
        assert_eq!(r.check_dirty_same_cargo(70_001.0, Some(70_000.0)), DirtyLoadOutcome::PreferWash);
    }

    /// Коэффициент потолка: > 1 допускает более дальнюю прямую погрузку, ≤ 0 отключает потолок.
    #[test]
    fn dirty_same_cargo_cap_ratio() {
        let mut r = BusinessRules { dirty_same_cargo_max_cost_ratio_to_wash: 1.5, ..Default::default() };
        assert_eq!(r.check_dirty_same_cargo(105_000.0, Some(70_000.0)), DirtyLoadOutcome::Allowed { reward_rub: 0.0 });
        assert_eq!(r.check_dirty_same_cargo(105_001.0, Some(70_000.0)), DirtyLoadOutcome::PreferWash);
        r.dirty_same_cargo_max_cost_ratio_to_wash = 0.0;
        assert!(!r.dirty_same_cargo_cap_enabled());
        assert_eq!(r.check_dirty_same_cargo(900_000.0, Some(70_000.0)), DirtyLoadOutcome::Allowed { reward_rub: 0.0 });
    }

    /// Нормализация: доля поощрения зажимается в [0, 1], стоимости — не отрицательные;
    /// надбавка промывочного маршрута следует за заданными стоимостями.
    #[test]
    fn dirty_same_cargo_params_parse_and_normalize() {
        let mut r: BusinessRules = serde_json::from_str(
            r#"{"WashProcedureCostRub": 12000, "EmptyRunAfterWashCostRub": 30000,
                "DirtySameCargoMaxCostRatioToWash": 1.2, "DirtySameCargoRewardShare": 1.5}"#,
        )
        .unwrap();
        r.normalize();
        assert_eq!(r.wash_path_surcharge_rub(), 42_000.0);
        assert_eq!(r.dirty_same_cargo_max_cost_ratio_to_wash, 1.2);
        assert_eq!(r.dirty_same_cargo_reward_share, 1.0, "доля > 1 зажимается в 1");
        // Промывочный маршрут 62 000 = тариф 20 000 + 42 000; поощрение 1 × (20 000 + 12 000).
        assert_eq!(r.check_dirty_same_cargo(50_000.0, Some(62_000.0)), DirtyLoadOutcome::Allowed { reward_rub: 32_000.0 });

        let mut r: BusinessRules = serde_json::from_str(
            r#"{"WashProcedureCostRub": -5, "EmptyRunAfterWashCostRub": -1, "DirtySameCargoRewardShare": -0.3}"#,
        )
        .unwrap();
        r.normalize();
        assert_eq!(r.wash_path_surcharge_rub(), 0.0);
        assert_eq!(r.dirty_same_cargo_reward_share, 0.0);
    }

    #[test]
    fn station_backlog_params_parse_and_default_off() {
        let r = BusinessRules::default();
        assert!(!r.station_backlog_enabled());
        let r: BusinessRules = serde_json::from_str("{}").unwrap();
        assert!(!r.station_backlog_enabled());
        // 0 → отключено.
        let r: BusinessRules = serde_json::from_str(r#"{"StationBacklogHardDays": 0}"#).unwrap();
        assert!(!r.station_backlog_enabled());
        let mut r: BusinessRules = serde_json::from_str(
            r#"{"StationBacklogHardDays": "5", "StationBacklogSoftDays": -2, "StationBacklogWaitPenaltyRubPerDay": 7000}"#,
        )
        .unwrap();
        r.normalize();
        assert!(r.station_backlog_enabled());
        assert_eq!(r.station_backlog_hard_days, Some(5));
        assert_eq!(r.station_backlog_soft_days, 0, "отрицательный мягкий порог зажимается в 0");
        assert_eq!(r.station_backlog_wait_penalty_rub_per_day, 7000.0);
    }

    #[test]
    fn gu12_check_defaults_to_enabled_and_can_be_disabled() {
        assert!(BusinessRules::default().gu12_check_enabled);
        // Список инотерриторий по умолчанию пуст — ГУ-12 не выполняется (защита от
        // непрочитанного business_rules.json: иначе срежет спрос на инотерриториях).
        assert!(!BusinessRules::default().gu12_ready());
        let r: BusinessRules = serde_json::from_str("{}").unwrap();
        assert!(r.gu12_check_enabled);
        assert!(!r.gu12_ready());
        let r: BusinessRules = serde_json::from_str(r#"{"Gu12CheckEnabled": false}"#).unwrap();
        assert!(!r.gu12_check_enabled);
        assert!(!r.gu12_ready());
        let mut r = BusinessRules::default();
        r.foreign_railways = ["КЗХ".into()].into_iter().collect();
        assert!(r.gu12_ready());
        r.gu12_check_enabled = false;
        assert!(!r.gu12_ready());
    }

    #[test]
    fn convention_check_defaults_to_enabled_and_can_be_disabled() {
        assert!(BusinessRules::default().convention_check_enabled);
        let r: BusinessRules = serde_json::from_str("{}").unwrap();
        assert!(r.convention_check_enabled);
        let r: BusinessRules = serde_json::from_str(r#"{"ConventionCheckEnabled": false}"#).unwrap();
        assert!(!r.convention_check_enabled);
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

    // -----------------------------------------------------------------------
    // Правило 7: горизонт вывода в ремонт
    // -----------------------------------------------------------------------

    /// По умолчанию 15 / 45; без списка инотерриторий все дороги — как российские.
    #[test]
    fn repair_days_defaults_and_foreign_list() {
        let r = BusinessRules::default();
        assert_eq!(r.repair_days_threshold, 15);
        assert_eq!(r.repair_days_threshold_foreign, 45);
        assert_eq!(r.repair_days_threshold_for("МСК"), 15);
        assert_eq!(r.repair_days_threshold_for("КЗХ"), 15, "пустой ForeignRailways — 45 не применяется");
        assert!(r.wagon_needs_repair(false, Some(14.9), "МСК"));
        assert!(!r.wagon_needs_repair(false, Some(15.0), "МСК"));
        assert!(!r.wagon_needs_repair(false, None, "МСК"));
        assert!(r.wagon_needs_repair(true, Some(100.0), "МСК"), "IsCarRepair безусловен");

        let mut r = BusinessRules::default();
        r.foreign_railways = ["КЗХ".into()].into_iter().collect();
        assert_eq!(r.repair_days_threshold_for("КЗХ"), 45);
        assert_eq!(r.repair_days_threshold_for(" кзх "), 15, "сравнение строгое, без нормализации регистра");
        assert!(r.wagon_needs_repair(false, Some(20.0), "КЗХ"));
        assert!(r.wagon_needs_repair(false, Some(44.9), "КЗХ"));
        assert!(!r.wagon_needs_repair(false, Some(45.0), "КЗХ"));
        assert!(!r.wagon_needs_repair(false, Some(20.0), "МСК"));
    }

    #[test]
    fn repair_days_params_parse_and_normalize() {
        let mut r: BusinessRules = serde_json::from_str(
            r#"{"RepairDaysThreshold": 15, "RepairDaysThresholdForeign": 10, "ForeignRailways": ["КЗХ"]}"#,
        )
        .unwrap();
        r.normalize();
        assert_eq!(r.repair_days_threshold_foreign, 15, "инотерриториальный порог не короче российского");

        let mut r: BusinessRules = serde_json::from_str(
            r#"{"RepairDaysThreshold": -3, "RepairDaysThresholdForeign": 0}"#,
        )
        .unwrap();
        r.normalize();
        assert_eq!(r.repair_days_threshold, 0);
        assert!(!r.wagon_needs_repair(false, Some(1.0), "МСК"), "0 — проверка по дням выключена");
        assert!(r.wagon_needs_repair(true, Some(1.0), "МСК"));
    }

    // -----------------------------------------------------------------------
    // Правило 8: иномойка и капризные дороги
    // -----------------------------------------------------------------------

    /// По умолчанию иномойки нет: вагоны с любой дороги остаются кандидатами в грязные.
    #[test]
    fn foreign_washed_defaults_off() {
        let r = BusinessRules::default();
        assert!(r.foreign_washed_roads.is_empty());
        assert!(!r.is_foreign_washed("КЗХ"));
        assert_eq!(r.foreign_washed_picky_surcharge("КЗХ", "МСК", true), 0.0);
        assert!(!r.foreign_washed_picky_active(100, 10));
    }

    /// Надбавка только при профиците, только иномойка → капризная дорога.
    #[test]
    fn foreign_washed_picky_surcharge_only_on_surplus() {
        let mut r = BusinessRules::default();
        r.foreign_washed_roads = ["КЗХ".into()].into_iter().collect();
        r.foreign_washed_picky_railways = ["МСК".into()].into_iter().collect();
        r.foreign_washed_picky_surcharge_rub = 30_000.0;
        assert_eq!(r.foreign_washed_picky_surplus_ratio, 1.2, "порог по умолчанию 1.2");
        assert!(r.foreign_washed_picky_active(20, 10));
        assert!(!r.foreign_washed_picky_active(10, 20));
        assert!(!r.foreign_washed_picky_active(10, 10));
        // Запас 1.2: 12 > 12 — нет, 13 > 12 — профицит.
        assert!(!r.foreign_washed_picky_active(12, 10));
        assert!(r.foreign_washed_picky_active(13, 10));
        // Спроса нет — любое предложение профицит.
        assert!(r.foreign_washed_picky_active(1, 0));
        assert_eq!(r.foreign_washed_picky_surcharge("КЗХ", "МСК", true), 30_000.0);
        assert_eq!(r.foreign_washed_picky_surcharge("КЗХ", "МСК", false), 0.0);
        assert_eq!(r.foreign_washed_picky_surcharge("КЗХ", "ЮВС", true), 0.0);
        assert_eq!(r.foreign_washed_picky_surcharge("СКВ", "МСК", true), 0.0);
    }

    /// Порог профицита разбирается из JSON; меньше 1 поднимается до 1, мусор — к умолчанию.
    #[test]
    fn foreign_washed_surplus_ratio_parse_and_normalize() {
        let mut r: BusinessRules =
            serde_json::from_str(r#"{"ForeignWashedPickySurplusRatio": 1.5}"#).unwrap();
        r.normalize();
        assert_eq!(r.foreign_washed_picky_surplus_ratio, 1.5);

        let mut r: BusinessRules =
            serde_json::from_str(r#"{"ForeignWashedPickySurplusRatio": 0.5}"#).unwrap();
        r.normalize();
        assert_eq!(r.foreign_washed_picky_surplus_ratio, 1.0);

        let mut r = BusinessRules { foreign_washed_picky_surplus_ratio: f64::NAN, ..Default::default() };
        r.normalize();
        assert_eq!(r.foreign_washed_picky_surplus_ratio, DEFAULT_FOREIGN_WASHED_PICKY_SURPLUS_RATIO);

        let r: BusinessRules = serde_json::from_str("{}").unwrap();
        assert_eq!(r.foreign_washed_picky_surplus_ratio, DEFAULT_FOREIGN_WASHED_PICKY_SURPLUS_RATIO);
    }

    // -----------------------------------------------------------------------
    // Поправка периода 1 по расстоянию (аддитивная)
    // -----------------------------------------------------------------------

    #[test]
    fn p1_distance_adjust_disabled_by_default() {
        let r = BusinessRules::default();
        assert!(!r.p1_distance_adjust_enabled());
        assert_eq!(r.p1_distance_adjust_rub(0), 0.0);
        assert_eq!(r.p1_distance_adjust_rub(9_000), 0.0);
        assert_eq!(r.p1_load_distance_adjust_rub(1, DemandPurpose::Load, 100), 0.0);
        let r: BusinessRules = serde_json::from_str("{}").unwrap();
        assert!(!r.p1_distance_adjust_enabled());
    }

    #[test]
    fn p1_distance_adjust_linear_and_capped() {
        let r = BusinessRules {
            p1_distance_neutral_km: 2_000,
            p1_distance_rub_per_km: 5.0,
            p1_distance_cap_rub: 8_000.0,
            ..Default::default()
        };
        assert!(r.p1_distance_adjust_enabled());
        assert!((r.p1_distance_adjust_rub(2_000)).abs() < 1e-12, "на d₀ поправки нет");
        assert!((r.p1_distance_adjust_rub(1_000) + 5_000.0).abs() < 1e-12, "ближе — бонус");
        assert!((r.p1_distance_adjust_rub(3_000) - 5_000.0).abs() < 1e-12, "дальше — надбавка");
        assert!((r.p1_distance_adjust_rub(0) + 8_000.0).abs() < 1e-12, "потолок снизу");
        assert!((r.p1_distance_adjust_rub(6_000) - 8_000.0).abs() < 1e-12, "потолок сверху");
        assert_eq!(r.p1_load_distance_adjust_rub(10, DemandPurpose::Load, 0), 0.0);
        assert_eq!(r.p1_load_distance_adjust_rub(1, DemandPurpose::Wash, 0), 0.0);
        assert!((r.p1_load_distance_adjust_rub(1, DemandPurpose::Load, 3_000) - 5_000.0).abs() < 1e-12);
    }

    #[test]
    fn p1_distance_adjust_disable_for_day1_mode() {
        let mut r = BusinessRules {
            p1_distance_neutral_km: 2_000,
            p1_distance_rub_per_km: 5.0,
            p1_distance_cap_rub: 15_000.0,
            ..Default::default()
        };
        assert!(r.p1_distance_adjust_enabled());
        r.disable_p1_distance_adjust();
        assert!(!r.p1_distance_adjust_enabled());
        assert_eq!(r.p1_load_distance_adjust_rub(1, DemandPurpose::Load, 0), 0.0);
        assert_eq!(r.p1_load_distance_adjust_rub(1, DemandPurpose::Load, 6_000), 0.0);
    }

    #[test]
    fn p1_distance_adjust_without_cap_is_unbounded() {
        let r = BusinessRules {
            p1_distance_neutral_km: 1_000,
            p1_distance_rub_per_km: 10.0,
            p1_distance_cap_rub: 0.0,
            ..Default::default()
        };
        assert!((r.p1_distance_adjust_rub(6_000) - 50_000.0).abs() < 1e-12);
        assert!((r.p1_distance_adjust_rub(0) + 10_000.0).abs() < 1e-12);
    }

    #[test]
    fn p1_distance_adjust_normalize_garbage() {
        let mut r = BusinessRules {
            p1_distance_neutral_km: -10,
            p1_distance_rub_per_km: f64::NAN,
            p1_distance_cap_rub: -5.0,
            ..Default::default()
        };
        r.normalize();
        assert_eq!(r.p1_distance_neutral_km, 0);
        assert_eq!(r.p1_distance_rub_per_km, 0.0);
        assert_eq!(r.p1_distance_cap_rub, 0.0);
        assert!(!r.p1_distance_adjust_enabled());

        let mut r: BusinessRules = serde_json::from_str(
            r#"{"P1DistanceNeutralKm": 2000, "P1DistanceRubPerKm": 5, "P1DistanceCapRub": 15000}"#,
        )
        .unwrap();
        r.normalize();
        assert!(r.p1_distance_adjust_enabled());
        assert_eq!(r.p1_distance_neutral_km, 2_000);
        assert_eq!(r.p1_distance_rub_per_km, 5.0);
        assert_eq!(r.p1_distance_cap_rub, 15_000.0);
    }
}

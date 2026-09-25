use std::collections::{HashMap, HashSet};

use crate::data::business_rules::{BusinessRules, DirtyLoadOutcome, RuleOutcome};
use crate::data::convention_index::{ArcTiming, ConventionIndex};
use crate::data::references::normalize_etsng_code;
use crate::data::station_backlog::StationBacklogIndex;
use crate::data::wash::{effective_etsng_for_wash_tariff, supply_needs_wash};
use crate::node::{DemandNode, DemandPurpose, SupplyNode, TariffNode};

// ---------------------------------------------------------------------------
// Константы ограничений
// ---------------------------------------------------------------------------

/// Минимальный допустимый размер партии вагонов, назначаемых с узлов
/// **станции массовой выгрузки** (`is_mass_unloading == true`) на узлы
/// одной станции погрузки. Значение 0 тоже допустимо (нет назначений между станциями вовсе).
///
/// Значение `x` на суммах дуг станция-станция должно удовлетворять: `x == 0 || x >= MIN_BATCH_FROM_MASS_STATION`.
pub const MIN_BATCH_FROM_MASS_STATION: i32 = 5;

/// Минимальный размер партии «средняя станция предложения → средне-крупная станция погрузки».
///
/// Бизнес-логика: подсыл по 1–2 вагона невыгоден клиенту — маневровые работы на станции
/// оплачиваются за подачу, а не за вагон. Аналог `_ASSIGN_LOW_BOUND_` из example.py.
///
/// Поток по паре станций должен удовлетворять: `x == 0 || x >= MIN_BATCH_TO_MIDDLE_DEMAND_STATION`.
pub const MIN_BATCH_TO_MIDDLE_DEMAND_STATION: i32 = 3;

/// Минимальное суммарное предложение на станции образования (все периоды),
/// при котором станция считается **средней** и попадает под ограничение
/// [`MIN_BATCH_TO_MIDDLE_DEMAND_STATION`]. Станции массовой выгрузки исключаются
/// (для них действует [`MIN_BATCH_FROM_MASS_STATION`]). Аналог `_SUPPLY_SIZE_BOUND_`.
pub const MIDDLE_SUPPLY_STATION_MIN_CARS: i32 = 10;

/// Минимальный суммарный Load-спрос на станции погрузки (без маршрутных отправок),
/// при котором станция считается **средне-крупной** и попадает под ограничение
/// [`MIN_BATCH_TO_MIDDLE_DEMAND_STATION`]. Аналог `_DEMAND_SIZE_BOUND_`.
pub const MIDDLE_DEMAND_STATION_MIN_CARS: i32 = 10;

/// Минимальный размер партии «станция образования → маршрутные узлы станции погрузки»
/// (`shipping_type == "Маршрутная"`). Аналог `_ROUTE_LOW_BOUND_` из example.py.
///
/// Маршрутная отправка формирует целый состав, поэтому подсыл меньшими партиями
/// не имеет смысла: поток по паре должен быть `0` или `>= MIN_BATCH_TO_ROUTE_DEMAND_STATION`.
///
/// Та же константа служит порогом отбора станций предложения (как в example.py:
/// `s_route_qty >= _ROUTE_LOW_BOUND_`): ограничение действует только для станций,
/// которые суммарно (периоды 1 и 10 вместе, **включая** массовые) могут собрать
/// партию. Станции с меньшим предложением шлют на маршрутные станции без ограничения.
pub const MIN_BATCH_TO_ROUTE_DEMAND_STATION: i32 = 10;

/// Штраф к тарифу (руб.) за каждые полные сутки выхода за допустимое окно срока подсыла
/// `[L - 3, U + 3]` для предложений с [`SupplyNode::supply_period`] **не равным** 10.
pub const PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB: f64 = 15_000.0;

/// Штраф к тарифу (руб.) за каждые полные сутки нарушения окна для предложений
/// с [`SupplyNode::supply_period`] == 10 (дислокация 2–10 суток).
///
/// Равен [`PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB`]. Раньше был вдвое выше
/// (30 000), но вместе со сдвигом окна на −5 сут. (`[L − 3 − 5, U + 3 − 5]`) это
/// делало почти любой ближний вагон дислокации дороже дальнего вагона периода 1:
/// на спрос периода 1 вагон дислокации со сроком ≥ 3 сут. всегда «опаздывает»,
/// и при 30 000/сут. надбавка достигала 100–200 тыс. — эквивалент тысяч км тарифа.
/// Сдвиг окна сохраняется (он отражает реальное позднее освобождение вагона),
/// ставка выравнена с периодом 1.
pub const PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_PERIOD10_RUB: f64 = 15_000.0;

/// Надбавка к стоимости дуг предложения с `supply_period == 10` (дислокация 2–10 суток).
///
/// Делает вагоны дислокации менее привлекательными для решателя по сравнению
/// с вагонами периода 1 (готовы сегодня). Если оба вагона могут закрыть один
/// узел спроса и разница в тарифе ≤ `PERIOD10_COST_SURCHARGE_RUB`, решатель
/// предпочтёт вагон периода 1.
///
/// Намеренно **мала** (символический tie-breaker): исторически была 120 000,
/// что заставляло модель посылать вагон периода 1 через всю страну вместо
/// ближнего вагона дислокации. Дальность подсыла периода 1 дополнительно
/// регулируется аддитивной поправкой по расстоянию (`P1Distance*` в
/// `business_rules.json`); жёсткий потолок — `MaxEmptyRunDistanceKm`.
pub const PERIOD10_COST_SURCHARGE_RUB: f64 = 2_000.0;

// Стоимость промывочного маршрута (промывка + порожний пробег после неё) и параметры
// правила 6 «грязный вагон под аналогичный груз» — в `BusinessRules`
// (`WashProcedureCostRub`, `EmptyRunAfterWashCostRub`, `DirtySameCargo*`).

// ---------------------------------------------------------------------------
// Дуга транспортной задачи
// ---------------------------------------------------------------------------

/// Дуга оптимизационной задачи: возможное назначение одного узла предложения
/// на один узел спроса.
///
/// Каждая дуга соответствует паре (SupplyNode, DemandNode), для которой
/// найден тариф. Совокупность всех дуг образует граф транспортной задачи,
/// на котором LP-солвер минимизирует суммарную стоимость перевозки.
#[derive(Debug, Clone)]
pub struct TaskArc {
    /// Порядковый номер дуги в плоском списке (используется как индекс LP-переменной).
    pub arc_id: usize,

    /// Позиция узла предложения в срезе `supply` (0-based).
    pub s_idx: usize,
    /// Позиция узла спроса в срезе `demand` (0-based).
    pub d_idx: usize,

    /// Код станции образования порожнего (откуда подсылаем).
    pub supply_station_code: String,
    /// Код станции погрузки (куда подсылаем).
    pub demand_station_code: String,

    /// Стоимость дуги для оптимизации, руб.: тариф + штраф за срок + надбавки
    /// (промывка + порожний пробег после промывки для Wash-дуг, period 10, бизнес-правила)
    /// ± поправка периода 1 по расстоянию (`P1Distance*`).
    pub cost: f64,
    /// Чистый тариф передислокации порожнего вагона между станциями дуги, руб. —
    /// без модельных штрафов и надбавок. Используется в отчётах (Excel/API): для
    /// Wash-дуги это только тариф до станции промывки, без стоимости промывки и
    /// последующего подсыла под погрузку.
    pub tariff_cost: f64,
    /// Расстояние, км.
    pub distance: i32,
    /// Нормативный срок подсыла, сут.
    pub delivery_days: i32,

    /// Срок подсыла в пределах окна `[L−3, U+3]` по периоду спроса без штрафа.
    /// со слабыми ограничениями поле не нужно
    pub period_ok: bool,
    /// Тип вагона совместим с требованиями узла спроса.
    pub car_type_ok: bool,
    /// Минимальный размер партии для группы дуг [`TaskArc::pair_key`].
    ///
    /// `0` — ограничения нет. Иначе суммарный поток по всем дугам группы должен быть
    /// `0` или `>= pair_min_batch`:
    /// - [`MIN_BATCH_TO_ROUTE_DEMAND_STATION`] — маршрутные узлы спроса
    ///   (`shipping_type == "Маршрутная"`), предложение со станции `>= 10` ваг.;
    /// - [`MIN_BATCH_FROM_MASS_STATION`] — предложение на станции массовой выгрузки;
    /// - [`MIN_BATCH_TO_MIDDLE_DEMAND_STATION`] — средняя станция предложения →
    ///   средне-крупная станция погрузки.
    ///
    /// На одной паре станций могут сосуществовать **две** группы: маршрутные узлы
    /// (B = 10) и немаршрутные (B = 3 или 0) — поэтому ключ группы включает порог.
    pub pair_min_batch: i32,

    /// Дуга не расходует потолок ГУ-12 узла спроса: вагон образовался на инотерритории
    /// ([`crate::data::BusinessRules::supply_exempt_from_gu12`]). Для промывки и при
    /// выключенном правиле 3 флаг не меняет модель (потолка на узле нет).
    pub gu12_exempt: bool,
}

/// Ключ группы дуг ограничения минимальной партии:
/// `(станция_предложения, станция_погрузки, порог_партии)`.
///
/// Порог входит в ключ, чтобы маршрутные и немаршрутные узлы одной станции
/// погрузки образовывали **разные** группы (как в example.py: route-ограничение
/// суммирует поток только по маршрутным узлам, dml/bulk — по остальным).
pub type PairKey = (String, String, i32);

impl TaskArc {
    /// Дуга участвует в ограничении минимальной партии на паре станций.
    #[inline]
    pub fn has_pair_min_batch(&self) -> bool {
        self.pair_min_batch > 0
    }

    /// Ключ группы ограничения минимальной партии для дуги.
    ///
    /// Имеет смысл только для дуг с [`TaskArc::has_pair_min_batch`].
    #[inline]
    pub fn pair_key(&self) -> PairKey {
        (
            self.supply_station_code.clone(),
            self.demand_station_code.clone(),
            self.pair_min_batch,
        )
    }
}

// ---------------------------------------------------------------------------
// Ограничения ДМЗИ (динамическая модель загрузки ж-д инфраструктуры)
// ---------------------------------------------------------------------------

/// Квоты ДМЗИ: `(нормализованный код дороги погрузки, период предложения 1|10)`
/// → максимум вагонов, которые можно подослать на дорогу.
///
/// Строится из [`crate::data::dmzi::DmziQuotas::to_limits`]:
/// период 1 — сумма `Normativ` за сутки 1–5, период 10 — за весь горизонт 7 суток.
pub type DmziLimits = HashMap<(String, u8), i32>;

/// Индекс квот ДМЗИ по дугам задачи.
///
/// Бакет — пара `(дорога погрузки, период предложения)`. Суммарный поток по всем
/// дугам бакета не должен превышать его лимит. Под квоту попадают **только дуги
/// на Load-узлы**: промывка не считается подсылом под погрузку.
#[derive(Debug, Clone)]
pub struct DmziIndex {
    /// Бакеты с лимитами; порядок стабильный (сортировка по ключу).
    pub buckets: Vec<((String, u8), i32)>,
    /// Позиция дуги в `arcs` → индекс бакета; `None` — дуга вне квот
    /// (не Load-узел или для дороги нет лимита).
    pub arc_bucket: Vec<Option<usize>>,
    /// Ключ бакета → его индекс в `buckets`.
    pos: HashMap<(String, u8), usize>,
}

impl DmziIndex {
    /// Строит индекс для конкретного набора дуг (позиции в `arc_bucket`
    /// соответствуют позициям в `arcs`).
    pub fn build(
        arcs: &[TaskArc],
        supply: &[SupplyNode],
        demand: &[DemandNode],
        limits: &DmziLimits,
    ) -> Self {
        let mut buckets: Vec<((String, u8), i32)> = limits
            .iter()
            .map(|(key, &limit)| (key.clone(), limit.max(0)))
            .collect();
        buckets.sort();

        let pos: HashMap<(String, u8), usize> = buckets
            .iter()
            .enumerate()
            .map(|(i, (key, _))| (key.clone(), i))
            .collect();

        let arc_bucket: Vec<Option<usize>> = arcs
            .iter()
            .map(|arc| {
                let d = &demand[arc.d_idx];
                if d.purpose != DemandPurpose::Load {
                    return None;
                }
                let key = (
                    crate::data::dmzi::normalize_railway(&d.railway_name),
                    supply[arc.s_idx].supply_period,
                );
                pos.get(&key).copied()
            })
            .collect();

        Self { buckets, arc_bucket, pos }
    }

    /// Индекс бакета по дороге погрузки и периоду предложения.
    pub fn bucket_for(&self, railway: &str, supply_period: u8) -> Option<usize> {
        self.pos
            .get(&(crate::data::dmzi::normalize_railway(railway), supply_period))
            .copied()
    }

    /// Вектор лимитов в порядке `buckets` (стартовые остатки квот).
    pub fn limits_vec(&self) -> Vec<i32> {
        self.buckets.iter().map(|(_, limit)| *limit).collect()
    }

    /// Использование бакетов по значениям дуговых переменных (порядок `arcs`).
    pub fn usage_from_arc_vals(&self, arc_vals: &[f64]) -> Vec<i32> {
        let mut used = vec![0_i32; self.buckets.len()];
        for (i, &q) in arc_vals.iter().enumerate() {
            let qi = q.round() as i32;
            if qi <= 0 {
                continue;
            }
            if let Some(b) = self.arc_bucket[i] {
                used[b] += qi;
            }
        }
        used
    }
}

// ---------------------------------------------------------------------------
// Построение дуг
// ---------------------------------------------------------------------------

/// Строит список **допустимых** дуг транспортной задачи.
///
/// В LP попадают только пары, для которых одновременно выполнены:
/// - найден тариф по ключу `(supply.station_to_code, demand.station_code)`;
/// - тип вагона совместим с требованиями спроса (`car_type_ok`) — **жёстко**;
/// - период спроса имеет табличные границы — иначе дуга отбрасывается жёстко.
///
/// Нарушение допустимого окна срока подсыла — **мягкое** для всех периодов предложения:
/// - период 1: окно `[L−3, U+3]`, штраф [`PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB`]/сут.
/// - период 10: окно `[L−3−5, U+3−5]` (сдвиг −5 сут.), штраф
///   [`PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_PERIOD10_RUB`]/сут. (та же ставка).
///
/// [`TaskArc::period_ok`] == `true` означает, что нарушения окна нет.
/// Неудовлетворённый спрос обрабатывается slack-переменными в [`super::lp::solve`].
///
/// **Бизнес-правила** (`rules`, см. [`BusinessRules`]) действуют только на дуги
/// **погрузки**; Wash-дуги не ограничиваются (станций промывки мало, грязный вагон
/// должен доехать до ближайшей):
/// - потолок дальности подсыла `MaxEmptyRunDistanceKm` — пара с тарифным расстоянием
///   больше порога не создаётся ([`PairOutcome::TooFar`]): порожний зерновоз не гонят
///   через всю страну (ДВС → центр); без потолка [`super::lp::PENALTY_UNMET`] (1 млн)
///   делает выгодной любую дугу дешевле миллиона;
/// - инотерритории — с российских дорог порожние туда не назначают, кроме явных
///   исключений с надбавкой ([`PairOutcome::ForeignTerritory`]);
/// - дефицитные дороги — порожние с них на другие дороги не забирают, кроме очень
///   короткого плеча с надбавкой ([`PairOutcome::DeficitRoadExport`]);
/// - загруженность станции погрузки (`backlog`, правило 4, см.
///   [`crate::data::station_backlog`]) — станция с очередью больше
///   `StationBacklogHardDays` суток работы закрыта во все периоды
///   ([`PairOutcome::StationOverloaded`]); при меньшей очереди вагон, приезжающий
///   раньше её рассасывания, ждёт: ожидаемые сутки погрузки сдвигаются (и проверяются
///   окном периода), за сутки ожидания начисляется `StationBacklogWaitPenaltyRubPerDay`;
/// - конвенции РЖД (`conventions`, правило 5) — действующая телеграмма закрывает
///   пару Load или Wash ([`PairOutcome::ConventionBan`]); ремонт и отстой
///   проверяются отдельно. «50% от плана» в JSON нет — жёсткий запрет;
/// - грязный вагон под аналогичный груз (правило 6, [`BusinessRules::check_dirty_same_cargo`])
///   — вагон из-под груза, требующего промывки, идёт под погрузку только того же ЕТСНГ
///   ([`PairOutcome::DirtyEtsngMismatch`] иначе), не дальше `k ×` промывочного маршрута
///   ([`PairOutcome::DirtyFarLoadPreferWash`]) и с поощрением — долей отложенной
///   промывки, снимаемой со стоимости дуги, чтобы при близких тарифах заявку на свой
///   груз получал грязный вагон, а не чистый.
///
/// Возвращает `(arcs, stats)`, где `stats` — счётчики для диагностики.
///
/// `tariffs` — тарифы до станций **погрузки** (как из АПИ).
/// `wash_tariffs` — тарифы до станций **промывки** с уже учтённой надбавкой
/// [`BusinessRules::wash_path_surcharge_rub`] (промывка + порожний пробег до погрузки),
/// ключ `(откуда, куда)`.
/// `backlog` — индекс загруженности станций ([`StationBacklogIndex::disabled`] — без правила 4).
/// `conventions` — индекс конвенций ([`ConventionIndex::disabled`] — без правила 5).
#[allow(clippy::too_many_arguments)]
pub fn build_task_arcs(
    supply: &[SupplyNode],
    demand: &[DemandNode],
    tariffs: &[TariffNode],
    wash_codes: &HashSet<String>,
    washed_empty_codes: &HashSet<String>,
    wash_tariffs: &HashMap<(String, String), TariffNode>,
    rules: &BusinessRules,
    backlog: &StationBacklogIndex,
    conventions: &ConventionIndex,
) -> (Vec<TaskArc>, ArcStats) {
    // Индекс тарифов погрузки: (код_откуда, код_куда) → TariffNode
    let tariff_index: HashMap<(&str, &str), &TariffNode> = tariffs
        .iter()
        .map(|t| ((t.station_from_code.as_str(), t.station_to_code.as_str()), t))
        .collect();

    // --- Классификация станций для ограничений минимальной партии ---
    //
    // Суммарное предложение по станциям (периоды 1 и 10 вместе) и множество
    // станций массовой выгрузки — общая база для средних и маршрутных классов.
    let mut supply_station_totals: HashMap<&str, i32> = HashMap::new();
    let mut mass_stations: HashSet<&str> = HashSet::new();
    for s in supply {
        *supply_station_totals.entry(s.station_to_code.as_str()).or_insert(0) += s.car_count;
        if s.is_mass_unloading {
            mass_stations.insert(s.station_to_code.as_str());
        }
    }

    // Средние станции предложения: суммарно >= MIDDLE_SUPPLY_STATION_MIN_CARS вагонов,
    // исключая станции массовой выгрузки — у тех своё ограничение MIN_BATCH_FROM_MASS_STATION.
    let middle_supply_stations: HashSet<&str> = supply_station_totals
        .iter()
        .filter(|(code, total)| {
            **total >= MIDDLE_SUPPLY_STATION_MIN_CARS && !mass_stations.contains(*code)
        })
        .map(|(code, _)| *code)
        .collect();

    // Станции предложения для маршрутного ограничения: суммарно
    // >= MIN_BATCH_TO_ROUTE_DEMAND_STATION вагонов, **включая** массовые
    // (example.py, s_route_stations: массовые не исключаются).
    let route_supply_stations: HashSet<&str> = supply_station_totals
        .iter()
        .filter(|(_, total)| **total >= MIN_BATCH_TO_ROUTE_DEMAND_STATION)
        .map(|(code, _)| *code)
        .collect();

    // Средне-крупные станции погрузки: суммарный Load-спрос без маршрутных отправок
    // >= MIDDLE_DEMAND_STATION_MIN_CARS вагонов.
    let middle_demand_stations: HashSet<&str> = {
        let mut totals: HashMap<&str, i32> = HashMap::new();
        for d in demand {
            if d.purpose == DemandPurpose::Load && !is_route_shipping(d) {
                *totals.entry(d.station_code.as_str()).or_insert(0) += d.car_count;
            }
        }
        totals
            .into_iter()
            .filter(|(_, total)| *total >= MIDDLE_DEMAND_STATION_MIN_CARS)
            .map(|(code, _)| code)
            .collect()
    };

    let mut arcs       = Vec::new();
    let mut no_tariff  = 0usize;
    let mut bad_period = 0usize;
    let mut bad_type   = 0usize;
    let mut dirty_etsng_mismatch = 0usize;
    let mut dirty_far_prefer_wash = 0usize;
    let mut too_far = 0usize;
    let mut foreign_territory = 0usize;
    let mut deficit_export = 0usize;
    let mut station_overloaded = 0usize;
    let mut convention_ban = 0usize;
    let mut convention_by_number: HashMap<String, usize> = HashMap::new();
    let mut arcs_period_penalized = 0usize;
    let mut arcs_rule_surcharged = 0usize;
    let mut arcs_backlog_wait = 0usize;
    let mut arcs_dirty_rewarded = 0usize;
    let mut dirty_reward_total_rub = 0.0_f64;
    let mut arcs_foreign_washed_picky = 0usize;
    let mut arcs_p1_distance_bonus = 0usize;
    let mut arcs_p1_distance_surcharge = 0usize;
    let mut p1_distance_bonus_total_rub = 0.0_f64;
    let mut p1_distance_surcharge_total_rub = 0.0_f64;
    let mut gu12_blocked = 0usize;

    // Порог правила 6 для грязных вагонов: минимальная стоимость промывочного маршрута
    // по станции образования (см. classify_pair). Считается один раз.
    let wash_min_cost = wash_route_min_cost_by_station(wash_tariffs);

    // Правило 8: капризная надбавка иномойки — только при профиците порожних к погрузке.
    let market_surplus = rules.market_surplus(supply, demand);

    for (s_idx, s) in supply.iter().enumerate() {
        let s_wash_min = wash_min_cost.get(s.station_to_code.as_str()).copied();
        for (d_idx, d) in demand.iter().enumerate() {
            // Жёсткие фильтры пары вынесены в classify_pair — та же логика
            // переиспользуется в диагностике незакрытого спроса.
            let (tariff, cost, period_ok, rule_surcharged, wait_days, dirty_reward_rub, p1_adjust) = match classify_pair(
                s,
                d,
                &tariff_index,
                wash_codes,
                washed_empty_codes,
                wash_tariffs,
                s_wash_min,
                rules,
                backlog,
                conventions,
                market_surplus,
            ) {
                PairOutcome::Feasible {
                    tariff, cost, period_ok, rule_surcharge_rub, wait_days, dirty_reward_rub,
                    p1_distance_adjust_rub,
                } => {
                    (tariff, cost, period_ok, rule_surcharge_rub > 0.0, wait_days, dirty_reward_rub, p1_distance_adjust_rub)
                }
                PairOutcome::NoTariff => { no_tariff += 1; continue; }
                PairOutcome::BadType => { bad_type += 1; continue; }
                PairOutcome::DirtyEtsngMismatch => { dirty_etsng_mismatch += 1; continue; }
                PairOutcome::DirtyFarLoadPreferWash => { dirty_far_prefer_wash += 1; continue; }
                PairOutcome::TooFar => { too_far += 1; continue; }
                PairOutcome::ForeignTerritory => { foreign_territory += 1; continue; }
                PairOutcome::DeficitRoadExport => { deficit_export += 1; continue; }
                PairOutcome::StationOverloaded => { station_overloaded += 1; continue; }
                PairOutcome::ConventionBan { rzd_number } => {
                    convention_ban += 1;
                    *convention_by_number.entry(rzd_number).or_insert(0) += 1;
                    continue;
                }
                PairOutcome::BadPeriod => { bad_period += 1; continue; }
            };
            // Правило 3: нулевой потолок ГУ-12 закрывает российский подсыл.
            // Вагон с инотерритории дугу сохраняет и потолок не расходует.
            let gu12_exempt = d.purpose == DemandPurpose::Load
                && rules.supply_exempt_from_gu12(&s.railway_to);
            if d.purpose == DemandPurpose::Load && !gu12_exempt && d.gu12_cap == Some(0) {
                gu12_blocked += 1;
                continue;
            }
            if !period_ok {
                arcs_period_penalized += 1;
            }
            if rule_surcharged {
                arcs_rule_surcharged += 1;
            }
            if wait_days > 0 {
                arcs_backlog_wait += 1;
            }
            if dirty_reward_rub > 0.0 {
                arcs_dirty_rewarded += 1;
                dirty_reward_total_rub += dirty_reward_rub;
            }
            if rules.foreign_washed_picky_surcharge(&s.railway_to, &d.railway_name, market_surplus) > 0.0 {
                arcs_foreign_washed_picky += 1;
            }
            if p1_adjust < 0.0 {
                arcs_p1_distance_bonus += 1;
                p1_distance_bonus_total_rub += -p1_adjust;
            } else if p1_adjust > 0.0 {
                arcs_p1_distance_surcharge += 1;
                p1_distance_surcharge_total_rub += p1_adjust;
            }

            // Ограничения минимальной партии действуют только для погрузки, не для промывки.
            // Приоритет классов: маршрутная отправка → массовая выгрузка → средние станции.
            let pair_min_batch = if d.purpose != DemandPurpose::Load {
                0
            } else if is_route_shipping(d) {
                // Маршрутный узел спроса: партия >= 10, если станция предложения
                // в принципе может её собрать (>= 10 ваг. суммарно). Станции с
                // меньшим предложением шлют без ограничения (example.py:
                // route-ограничение строится только для s_route_stations_filtered).
                if route_supply_stations.contains(s.station_to_code.as_str()) {
                    MIN_BATCH_TO_ROUTE_DEMAND_STATION
                } else {
                    0
                }
            } else if s.is_mass_unloading {
                MIN_BATCH_FROM_MASS_STATION
            } else if middle_supply_stations.contains(s.station_to_code.as_str())
                && middle_demand_stations.contains(d.station_code.as_str())
            {
                MIN_BATCH_TO_MIDDLE_DEMAND_STATION
            } else {
                0
            };

            // Чистый тариф для отчёта: в wash_tariffs стоимость уже содержит надбавку
            // промывочного маршрута (промывка + порожний пробег после промывки) — снимаем её.
            let tariff_cost = if d.purpose == DemandPurpose::Wash {
                (tariff.cost - rules.wash_path_surcharge_rub()).max(0.0)
            } else {
                tariff.cost
            };

            arcs.push(TaskArc {
                arc_id: arcs.len(),
                s_idx,
                d_idx,
                supply_station_code: s.station_to_code.clone(),
                demand_station_code: d.station_code.clone(),
                cost,
                tariff_cost,
                distance:          tariff.distance,
                delivery_days:     tariff.period_of_delivery,
                period_ok,
                car_type_ok:       true,
                pair_min_batch,
                gu12_exempt,
            });
        }
    }

    let stats = ArcStats {
        total_pairs: supply.len() * demand.len(),
        no_tariff,
        bad_period,
        bad_type,
        dirty_etsng_mismatch,
        dirty_far_prefer_wash,
        too_far,
        foreign_territory,
        deficit_export,
        station_overloaded,
        convention_ban,
        convention_by_number,
        feasible: arcs.len(),
        arcs_period_penalized,
        arcs_rule_surcharged,
        arcs_backlog_wait,
        arcs_dirty_rewarded,
        dirty_reward_total_rub,
        arcs_foreign_washed_picky,
        arcs_p1_distance_bonus,
        arcs_p1_distance_surcharge,
        p1_distance_bonus_total_rub,
        p1_distance_surcharge_total_rub,
        gu12_blocked,
    };

    (arcs, stats)
}

/// Исход классификации пары `(supply, demand)` жёсткими фильтрами построения дуг.
///
/// Используется одновременно в [`build_task_arcs`] (создание дуг + статистика) и в
/// [`crate::solver::diagnose::diagnose_unmet_demand`] (разбор причин незакрытого
/// спроса). Единый источник логики гарантирует, что счётчики отбраковки в обоих
/// местах не разойдутся.
pub enum PairOutcome<'a> {
    /// Пара допустима — дуга создаётся. `cost` уже включает тариф, штраф за срок,
    /// надбавку period 10, надбавки бизнес-правил (`rule_surcharge_rub`) и за вычетом
    /// поощрения правила 6 (`dirty_reward_rub`); `period_ok` == `true`, если окно
    /// срока не нарушено.
    Feasible {
        tariff: &'a TariffNode,
        cost: f64,
        period_ok: bool,
        rule_surcharge_rub: f64,
        /// Сутки ожидания погрузки на станции из-за очереди (правило 4, мягкая часть);
        /// `0` — вагон приезжает не раньше, чем очередь рассосётся.
        wait_days: i32,
        /// Поощрение правила 6 (руб.), снятое со стоимости дуги «грязный → тот же
        /// ЕТСНГ»; `0` — вагон чистый или поощрение выключено.
        dirty_reward_rub: f64,
        /// Поправка периода 1 по расстоянию (руб., со знаком): `< 0` — бонус ближнему
        /// подсылу, `> 0` — надбавка дальнему; `0` — период 10, промывка или выключено.
        p1_distance_adjust_rub: f64,
    },
    /// Нет тарифа (для Wash также: вагон не требует промывки либо нет wash-тарифа).
    NoTariff,
    /// Несовместим тип вагона.
    BadType,
    /// Правило 6: грязный вагон → погрузка с несовпадающим ЕТСНГ (без промывки запрещено).
    DirtyEtsngMismatch,
    /// Правило 6: грязный вагон → погрузка аналогичного груза дороже `k ×` промывочного
    /// маршрута: дальний подсыл под тот же груз не делаем, вагон должен идти в промывку.
    DirtyFarLoadPreferWash,
    /// Погрузка дальше потолка расстояния подсыла (`MaxEmptyRunDistanceKm`):
    /// дальний порожний подсыл не практикуется, дуга не создаётся.
    TooFar,
    /// Бизнес-правило 1: подсыл порожнего на инотерриторию с этой дороги запрещён.
    ForeignTerritory,
    /// Бизнес-правило 2: вывоз порожнего с дефицитной дороги на плечо длиннее допустимого.
    DeficitRoadExport,
    /// Бизнес-правило 4: станция погрузки закрыта — вагонов на ней не меньше
    /// `StationBacklogHardDays` суток работы (`Q ≥ K_hard · C`).
    StationOverloaded,
    /// Бизнес-правило 5: конвенция РЖД запрещает пару (номер телеграммы).
    ConventionBan { rzd_number: String },
    /// Период спроса не имеет табличных границ (жёсткая отбраковка по сроку).
    BadPeriod,
}

/// Классифицирует пару `(supply, demand)` теми же жёсткими фильтрами, что и
/// [`build_task_arcs`]: тариф → грязный ЕТСНГ (правило 6; иномойка правила 8 не грязная)
/// → тип вагона → потолок расстояния → бизнес-правила дорог (инотерритории, дефицитные
/// дороги, капризная надбавка иномойки при профиците — правило 8) → конвенции РЖД
/// (правило 5) → загруженность станции (правило 4) → окно срока → потолок и поощрение
/// грязного вагона под свой груз (правило 6). Фильтры расстояния, дорог и
/// загруженности действуют только на дуги погрузки; конвенции — на Load и Wash.
///
/// `tariff_index` — индекс тарифов погрузки `(код_откуда, код_куда) → тариф`.
/// `wash_tariffs` — тарифы до промывки с уже учтённой надбавкой
/// [`BusinessRules::wash_path_surcharge_rub`].
/// `wash_route_min_cost` — минимальная стоимость промывочного маршрута со станции
/// образования вагона ([`wash_route_min_cost_by_station`]), `None` — промывка недоступна.
/// `rules` — бизнес-правила ([`BusinessRules::default()`] — без ограничений дорог,
/// правило 6 с прежними константами и без поощрения, правило 8 выключено).
/// `backlog` — загруженность станций погрузки ([`StationBacklogIndex::disabled`] — без правила 4).
/// `conventions` — конвенции РЖД ([`ConventionIndex::disabled`] — без правила 5).
/// `market_surplus` — профицит порожних к погрузке ([`BusinessRules::foreign_washed_picky_active`]);
/// без него капризная надбавка правила 8 не применяется.
#[allow(clippy::too_many_arguments)]
pub fn classify_pair<'a>(
    s: &SupplyNode,
    d: &DemandNode,
    tariff_index: &HashMap<(&str, &str), &'a TariffNode>,
    wash_codes: &HashSet<String>,
    washed_empty_codes: &HashSet<String>,
    wash_tariffs: &'a HashMap<(String, String), TariffNode>,
    wash_route_min_cost: Option<f64>,
    rules: &BusinessRules,
    backlog: &StationBacklogIndex,
    conventions: &ConventionIndex,
    market_surplus: bool,
) -> PairOutcome<'a> {
    // Грязный вагон, едущий под погрузку аналогичного груза (Load + same ЕТСНГ).
    // Для такой пары правило 6 применяет потолок и поощрение: см. ниже после расчёта стоимости.
    let mut dirty_load = false;
    let tariff: &TariffNode = match d.purpose {
        DemandPurpose::Wash => {
            // Вагоны с дорогой образования из ForeignWashedRoads — не грязные
            // (правило 8: клиент обязан вернуть вагон чистым с инотерритории).
            // Вагоны с текущим кодом из WashedEmptyEtsngCodes уже прошли промывку.
            if !supply_needs_wash(s, wash_codes, &rules.foreign_washed_roads, washed_empty_codes) {
                return PairOutcome::NoTariff;
            }
            let key = (s.station_to_code.clone(), d.station_code.clone());
            match wash_tariffs.get(&key) {
                Some(t) => t,
                None => return PairOutcome::NoTariff,
            }
        }
        DemandPurpose::Load => {
            // Правило 6, жёсткая часть: вагон из-под груза, требующего промывки
            // (и не освобождённый правилом 8 / ForeignWashedRoads), может идти под погрузку
            // ТОЛЬКО под тот же ЕТСНГ. Альтернатива — маршрут через узел промывки.
            if supply_needs_wash(s, wash_codes, &rules.foreign_washed_roads, washed_empty_codes) {
                let supply_etsng = effective_etsng_for_wash_tariff(s);
                let demand_etsng = d.etsng.as_deref().map(normalize_etsng_code);
                match (supply_etsng, demand_etsng) {
                    (Some(se), Some(de)) if se == de => { dirty_load = true; } // ЕТСНГ совпадает → дуга разрешена
                    _ => return PairOutcome::DirtyEtsngMismatch,
                }
            }
            let key = (s.station_to_code.as_str(), d.station_code.as_str());
            match tariff_index.get(&key) {
                Some(t) => *t,
                None => return PairOutcome::NoTariff,
            }
        }
    };

    if !car_type_compatible(s.car_type.as_deref(), d.car_type.as_deref()) {
        return PairOutcome::BadType;
    }

    // Правило 5: конвенция РЖД (Load и Wash). Порожний проверяется на окне
    // «отправление…прибытие», груз — на окне погрузки по периоду спроса
    // (см. `ConventionIndex`: конвенция — запрет приёма к перевозке в период действия).
    let dispatch_day = supply_release_shift_days(s.supply_period);
    let timing = ArcTiming {
        dispatch_day,
        arrival_day: dispatch_day + tariff.period_of_delivery,
        load_window: match d.purpose {
            DemandPurpose::Load => demand_period_day_bounds(d.period),
            DemandPurpose::Wash => None,
        },
    };
    if let Some(rule) = conventions.ban_for_arc(s, d, timing) {
        return PairOutcome::ConventionBan {
            rzd_number: rule.rzd_number.clone(),
        };
    }

    // --- Бизнес-правила (только погрузка) ---
    let mut rule_surcharge_rub = 0.0_f64;
    if d.purpose == DemandPurpose::Load {
        // Потолок дальности порожнего подсыла. Дальний подсыл (ДВС → центр) в бизнесе
        // не практикуется: без этого фильтра PENALTY_UNMET делает выгодной любую дугу
        // дешевле 1 млн, и модель везёт вагон через всю страну вместо того, чтобы
        // взять ближний или оставить заявку.
        if rules
            .max_empty_run_distance_km
            .is_some_and(|max_km| tariff.distance > max_km)
        {
            return PairOutcome::TooFar;
        }
        // Правила дорог: инотерритории и вывоз с дефицитных дорог.
        match rules.check_load_pair(&s.railway_to, &d.railway_name, tariff.distance) {
            RuleOutcome::Allowed { surcharge_rub } => rule_surcharge_rub = surcharge_rub,
            RuleOutcome::ForeignTerritory => return PairOutcome::ForeignTerritory,
            RuleOutcome::DeficitExport => return PairOutcome::DeficitRoadExport,
        }
        // Правило 8: при профиците капризная дорога не любит иномойку — надбавка к тарифу.
        rule_surcharge_rub +=
            rules.foreign_washed_picky_surcharge(&s.railway_to, &d.railway_name, market_surplus);
    }

    // --- Правило 4: загруженность станции погрузки (только погрузка) ---
    // Закрытая станция (Q ≥ K_hard·C) — дуги нет. Иначе вагон, прибывающий раньше
    // рассасывания очереди, ждёт: сутки ожидания прибавляются к сроку подсыла и
    // проверяются окном периода ниже, плюс штраф за простой.
    let mut wait_days = 0_i32;
    if let Some(b) = backlog.get(&d.station_code).filter(|_| d.purpose == DemandPurpose::Load) {
        if b.closed {
            return PairOutcome::StationOverloaded;
        }
        let arrival_day = supply_release_shift_days(s.supply_period) + tariff.period_of_delivery;
        wait_days = b.wait_days(arrival_day);
    }

    let penalty_rate = if s.supply_period == 10 {
        PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_PERIOD10_RUB
    } else {
        PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB
    };
    // Окном периода проверяются ожидаемые сутки погрузки: срок подсыла + ожидание в очереди.
    let Some(violation_days) = delivery_window_violation_days(
        tariff.period_of_delivery + wait_days,
        d.period,
        s.supply_period,
    ) else {
        return PairOutcome::BadPeriod;
    };
    let period_ok = violation_days == 0;
    let mut cost = tariff.cost
        + violation_days as f64 * penalty_rate
        + rule_surcharge_rub
        + wait_days as f64 * backlog.wait_penalty_rub_per_day();
    // надбавка к стоимости дуг period=10 для приоритизации period=1.
    if s.supply_period == 10 {
        cost += PERIOD10_COST_SURCHARGE_RUB;
    }

    // --- Правило 6: грязный вагон под аналогичный груз — потолок и поощрение ---
    // Потолок: дальний подсыл порожнего под погрузку того же груза без промывки не
    // практикуется — если прямая погрузка дороже `k ×` промывочного маршрута (тариф до
    // промывки + надбавка: промывка + порожний пробег под погрузку), вагон должен идти
    // в промывку; дугу не создаём, пара относится к причине DirtyFarLoadPreferWash.
    // Потолок действует только когда промывка вагону вообще доступна (есть wash-тариф):
    // иначе прямая погрузка — единственный шанс закрыть спрос, и её сохраняем.
    // Поощрение: со стоимости снимается доля отложенной промывки (тариф до ближайшей
    // промывки + сама промывка) — столько грязный вагон «должен» системе, если сегодня
    // не встанет под свой груз; чистый вагон-конкурент такого долга не несёт. Потолок
    // сравнивается со стоимостью ДО поощрения; после поощрения стоимость не ниже нуля.
    let mut dirty_reward_rub = 0.0_f64;
    if dirty_load {
        match rules.check_dirty_same_cargo(cost, wash_route_min_cost) {
            DirtyLoadOutcome::PreferWash => return PairOutcome::DirtyFarLoadPreferWash,
            DirtyLoadOutcome::Allowed { reward_rub } => {
                dirty_reward_rub = reward_rub.min(cost).max(0.0);
                cost -= dirty_reward_rub;
            }
        }
    }

    // --- Поправка периода 1 по расстоянию (гипотеза «ближнее — сегодняшним, дальнее —
    // дислокации») --- Прибавляется ПОСЛЕ правила 6: потолок «не дороже промывочного
    // маршрута» сравнивает реальный тариф с реальным маршрутом, поправка в него не идёт.
    // Только модельная стоимость; отчётный tariff_cost остаётся чистым тарифом АПИ.
    let p1_distance_adjust_rub =
        rules.p1_load_distance_adjust_rub(s.supply_period, d.purpose, tariff.distance);
    cost = (cost + p1_distance_adjust_rub).max(0.0);

    PairOutcome::Feasible {
        tariff,
        cost,
        period_ok,
        rule_surcharge_rub,
        wait_days,
        dirty_reward_rub,
        p1_distance_adjust_rub,
    }
}

/// Минимальная стоимость промывочного маршрута по станциям образования.
///
/// Ключ — код станции дислокации порожнего (`SupplyNode::station_to_code`),
/// значение — минимальный `cost` среди всех wash-тарифов из этой станции
/// (тариф уже включает надбавку [`BusinessRules::wash_path_surcharge_rub`]).
/// Порог правила 6 в [`classify_pair`]: потолок дальности и база поощрения для
/// грязных вагонов под аналогичный груз.
pub fn wash_route_min_cost_by_station(
    wash_tariffs: &HashMap<(String, String), TariffNode>,
) -> HashMap<String, f64> {
    let mut min_cost: HashMap<String, f64> = HashMap::new();
    for ((from_code, _to_code), t) in wash_tariffs {
        min_cost
            .entry(from_code.clone())
            .and_modify(|c| {
                if t.cost < *c {
                    *c = t.cost;
                }
            })
            .or_insert(t.cost);
    }
    min_cost
}

/// Диагностические счётчики из [`build_task_arcs`].
#[derive(Debug)]
pub struct ArcStats {
    /// Всего пар (supply × demand).
    pub total_pairs: usize,
    /// Пар без тарифа.
    pub no_tariff:  usize,
    /// Пар отсеяно по сроку подсыла (только жёсткий режим: нет границ периода или `supply_period == 10`).
    pub bad_period: usize,
    /// Пар с несовместимым типом вагона.
    pub bad_type:   usize,
    /// Пар «грязный» вагон → погрузка с несовпадающим ЕТСНГ (правило 6: запрещено без промывки).
    pub dirty_etsng_mismatch: usize,
    /// Пар «грязный» вагон → погрузка аналогичного груза дороже промывки (правило 6: предпочтена промывка).
    pub dirty_far_prefer_wash: usize,
    /// Пар погрузки дальше потолка расстояния подсыла (`MaxEmptyRunDistanceKm`).
    pub too_far: usize,
    /// Пар погрузки, запрещённых правилом инотерриторий (с этой дороги туда не назначают).
    pub foreign_territory: usize,
    /// Пар погрузки, запрещённых правилом дефицитных дорог (вывоз на длинное плечо).
    pub deficit_export: usize,
    /// Пар погрузки на закрытые станции (правило 4: очередь не меньше `K_hard` суток работы).
    pub station_overloaded: usize,
    /// Пар, запрещённых конвенцией РЖД (правило 5).
    pub convention_ban: usize,
    /// Сколько пар закрыла каждая телеграмма (номер → число).
    pub convention_by_number: HashMap<String, usize>,
    /// Допустимых дуг (вошли в LP).
    pub feasible:   usize,
    /// Дуг с ненулевым штрафом за срок подсыла (вне окна `[L−3, U+3]` с учётом сдвига периода 10).
    pub arcs_period_penalized: usize,
    /// Допустимых дуг с надбавкой по бизнес-правилам (исключение для инотерритории,
    /// короткий вывоз с дефицитной дороги, капризная иномойка правила 8).
    pub arcs_rule_surcharged: usize,
    /// Допустимых дуг с ожиданием в очереди станции (правило 4, мягкая часть).
    pub arcs_backlog_wait: usize,
    /// Допустимых дуг «грязный → тот же ЕТСНГ» с поощрением правила 6 (стоимость снижена).
    pub arcs_dirty_rewarded: usize,
    /// Суммарное поощрение по этим дугам (руб.) — для оценки масштаба скидки в логе.
    pub dirty_reward_total_rub: f64,
    /// Допустимых дуг погрузки с надбавкой правила 8 (иномойка → капризная дорога при профиците).
    pub arcs_foreign_washed_picky: usize,
    /// Дуг погрузки периода 1 ближе нейтральной дальности — с бонусом поправки по расстоянию.
    pub arcs_p1_distance_bonus: usize,
    /// Дуг погрузки периода 1 дальше нейтральной дальности — с надбавкой поправки по расстоянию.
    pub arcs_p1_distance_surcharge: usize,
    /// Суммарный бонус по этим дугам (руб., положительное число).
    pub p1_distance_bonus_total_rub: f64,
    /// Суммарная надбавка по этим дугам (руб.).
    pub p1_distance_surcharge_total_rub: f64,
    /// Пар погрузки с российских дорог в узел с нулевым потолком ГУ-12: дуга не строится.
    /// Вагоны с инотерриторий в этот счётчик не попадают.
    pub gu12_blocked: usize,
}

// ---------------------------------------------------------------------------
// Вспомогательные функции
// ---------------------------------------------------------------------------

/// Сутки погрузки по плановому периоду спроса: нижняя и верхняя граница включительно.
///
/// Значения соответствуют 0-based смещениям в [`crate::data::demand`]:
/// `DEMAND_PERIODS = [(0,4), (5,7), (8,9), (10,14)]`.
///
/// - Период 1: сут. 0–4  (сегодня + 0..4)
/// - Период 2: сут. 5–7
/// - Период 3: сут. 8–9
/// - Период 4: сут. 10–14
pub(crate) fn demand_period_day_bounds(period: u8) -> Option<(i32, i32)> {
    match period {
        1 => Some((0, 4)),
        2 => Some((5, 7)),
        3 => Some((8, 9)),
        4 => Some((10, 14)),
        _ => None,
    }
}

/// Допустим ли нормативный срок подсыла (`delivery_days`, сут.) для пары спрос/предложение.
///
/// Правило: по границам окна погрузки `[L, U]` допускается прибытие, если срок подсыла
/// попадает в `[L - 3, U + 3]` (трое суток раньше нижней границы и трое суток позже верхней,
/// граничные сутки периода входят в окно погрузки).
///
/// Для предложения с [`SupplyNode::supply_period`] == 10 (дислокация 2–10 суток) порожние
/// образуются на **5 суток позже**, чем у периода 1; то же окно для срока подсыла сдвигается
/// на −5 суток: проверяется `[L - 3 - 5, U + 3 - 5]`.
// pub(crate) fn delivery_period_ok(
//     delivery_days: i32,
//     demand_period: u8,
//     supply_period: u8,
// ) -> bool {
//     let Some((l, u)) = demand_period_day_bounds(demand_period) else {
//         return false;
//     };
//     let mut min_days = l - 3;
//     let mut max_days = u + 3;
//     if supply_period == 10 {
//         min_days -= 5;
//         max_days -= 5;
//     }
//     delivery_days >= min_days && delivery_days <= max_days
// }

/// Число полных суток, на которое `delivery_days` выходит за допустимое окно по периоду спроса.
///
/// Окно для `supply_period != 10`: `[L − 3, U + 3]`.
/// Окно для `supply_period == 10`: `[L − 3 − 5, U + 3 − 5]` (сдвиг −5 сут., т.к.
/// порожние из дислокации освобождаются в среднем на 5 суток позже).
///
/// Возвращает `None`, если период спроса не имеет табличных границ L, U.
fn delivery_window_violation_days(
    delivery_days: i32,
    demand_period: u8,
    supply_period:  u8,
) -> Option<i32> {
    let (l, u) = demand_period_day_bounds(demand_period)?;
    let shift    = supply_release_shift_days(supply_period);
    let min_days = l - 3 - shift;
    let max_days = u + 3 - shift;
    if delivery_days < min_days {
        Some(min_days - delivery_days)
    } else if delivery_days > max_days {
        Some(delivery_days - max_days)
    } else {
        Some(0)
    }
}

/// Сутки (от сегодня), когда порожний из узла предложения освобождается для подсыла:
/// `0` для предложения периода 1 (АПИ, готов сегодня), `5` для дислокации
/// (`supply_period == 10`, вагоны освобождаются в среднем на 5 суток позже).
/// Тот же сдвиг применяется к окну срока в [`delivery_window_violation_days`] и к
/// суткам прибытия на станцию для правил 4 и 5.
pub(crate) fn supply_release_shift_days(supply_period: u8) -> i32 {
    if supply_period == 10 { 5 } else { 0 }
}

/// Совместимость типа вагона с требованиями узла спроса.
///
/// - Спрос "БКТ" → предложение тоже должно быть "БКТ".
/// - Спрос "Прочие" / None → принимается любой тип вагона.
fn car_type_compatible(supply_type: Option<&str>, demand_type: Option<&str>) -> bool {
    match demand_type {
        Some(dt) if dt == "БКТ" => supply_type == Some("БКТ"),
        _ => true,
    }
}

/// Узел спроса относится к маршрутной отправке (`shipping_type == "Маршрутная"`).
///
/// Маршрутные станции исключаются из ограничения средних станций — для них
/// предусмотрено отдельное ограничение партии (вне текущего скоупа).
fn is_route_shipping(d: &DemandNode) -> bool {
    d.shipping_type.as_deref().map(str::trim) == Some("Маршрутная")
}

// ---------------------------------------------------------------------------
// Проверка ограничения минимальной партии на уровне пары станций
// ---------------------------------------------------------------------------

/// Возвращает ключи групп [`PairKey`], для которых суммарный поток нарушает
/// ограничение минимальной партии: `0 < total < pair_min_batch` (порог — третий
/// элемент ключа).
///
/// Учитываются дуги всех классов: массовая выгрузка, средние станции, маршрутные
/// отправки. Маршрутные и немаршрутные узлы одной станции погрузки — разные группы.
///
/// Принимает итератор `(arc_id, quantity)` — не зависит от конкретного типа назначения,
/// что позволяет использовать функцию как из `greedy.rs`, так и из `alns.rs`.
///
/// `arc_id` должен соответствовать индексу в срезе `arcs` (`arc.arc_id == index`).
pub fn collect_pair_min_batch_violations(
    flow: impl Iterator<Item = (usize, i32)>,
    arcs: &[TaskArc],
) -> Vec<PairKey> {
    // Ключ группы → суммарный поток.
    let mut totals: HashMap<(&str, &str, i32), i32> = HashMap::new();
    for (arc_id, quantity) in flow {
        let arc = &arcs[arc_id];
        if arc.has_pair_min_batch() {
            *totals
                .entry((
                    arc.supply_station_code.as_str(),
                    arc.demand_station_code.as_str(),
                    arc.pair_min_batch,
                ))
                .or_insert(0) += quantity;
        }
    }
    totals
        .into_iter()
        .filter(|((_, _, min_batch), total)| *total > 0 && *total < *min_batch)
        .map(|((s, d, b), _)| (s.to_string(), d.to_string(), b))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Короткие алиасы констант: размеры узлов в тестах выводятся из них,
    // чтобы тесты не ломались при настройке порогов.
    const S_MID: i32 = MIDDLE_SUPPLY_STATION_MIN_CARS;
    const D_MID: i32 = MIDDLE_DEMAND_STATION_MIN_CARS;
    const B_MID: i32 = MIN_BATCH_TO_MIDDLE_DEMAND_STATION;
    const B_ROUTE: i32 = MIN_BATCH_TO_ROUTE_DEMAND_STATION;

    /// Ожидаемый порог дуги на маршрутный узел при данном суммарном предложении станции:
    /// действует только если станция может собрать маршрутную партию.
    fn expected_route_batch(supply_total: i32) -> i32 {
        if supply_total >= B_ROUTE { B_ROUTE } else { 0 }
    }

    fn dummy_supply(count: i32, station_code: &str, period: u8, mass: bool) -> SupplyNode {
        SupplyNode {
            s_id: 0,
            kind: crate::node::CarKind::Free,
            car_count: count,
            station_to: String::new(),
            station_to_code: station_code.to_string(),
            railway_to: String::new(),
            railway_to_code: None,
            railway_part_to: None,
            car_type: Some("Прочие".to_string()),
            etsng: None,
            etsng_name: None,
            repair_status: crate::node::RepairStatus::Ok,
            status: None,
            supply_period: period,
            car_numbers: vec![],
            stations_from: vec![],
            stations_from_code: vec![],
            railways_from: vec![],
            railways_from_code: vec![],
            railways_part_from: vec![],
            is_mass_unloading: mass,
            prev_etsngs: vec![],
            prev_etsng_names: vec![],
        }
    }

    fn dummy_demand(count: i32, station_code: &str, shipping_type: Option<&str>) -> DemandNode {
        DemandNode {
            d_id: 0,
            purpose: DemandPurpose::Load,
            period: 1,
            station_name: String::new(),
            station_code: station_code.to_string(),
            railway_name: String::new(),
            railway_code: None,
            railway_part: None,
            station_to_name: None,
            station_to_code: None,
            railway_to_name: None,
            railway_to_code: None,
            railway_to_part: None,
            sender: None,
            sender_okpo: None,
            sender_tgnl: None,
            client: None,
            customer_okpo: None,
            recipient: None,
            loader_to_okpo: None,
            gng_cargo: None,
            etsng: None,
            request_numbers: None,
            request_dates: None,
            gu12_number: None,
            shipping_type: shipping_type.map(str::to_string),
            car_type: Some("Прочие".to_string()),
            car_count: count,
            gu12_cap: None,
            cars_on_station: 0,
        }
    }

    fn dummy_tariff(from: &str, to: &str) -> TariffNode {
        TariffNode {
            station_from: String::new(),
            station_from_code: from.to_string(),
            railway_from: String::new(),
            railway_from_code: 0,
            station_to: String::new(),
            station_to_code: to.to_string(),
            railway_to: String::new(),
            railway_to_code: 0,
            distance: 100,
            period_of_delivery: 1,
            cost: 1_000.0,
            actual_date: Default::default(),
        }
    }

    fn build(supply: &[SupplyNode], demand: &[DemandNode], tariffs: &[TariffNode]) -> Vec<TaskArc> {
        let (arcs, _) = build_task_arcs(
            supply,
            demand,
            tariffs,
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        arcs
    }

    fn rules_max_km(km: i32) -> BusinessRules {
        BusinessRules { max_empty_run_distance_km: Some(km), ..Default::default() }
    }

    /// Правила дорог для тестов: инотерритории КЗХ/УЗБ (КЗХ ← ОКТ с надбавкой 50k,
    /// КЗХ ← УЗБ без надбавки), дефицитная МСК (вывоз ≤ 300 км, надбавка 30k).
    fn rules_roads() -> BusinessRules {
        use crate::data::business_rules::ForeignException;
        BusinessRules {
            foreign_railways: ["КЗХ", "УЗБ"].iter().map(|s| s.to_string()).collect(),
            foreign_exceptions: vec![
                ForeignException {
                    demand_railway: "КЗХ".into(),
                    from_railways: ["ОКТ"].iter().map(|s| s.to_string()).collect(),
                    surcharge_rub: 50_000.0,
                },
                ForeignException {
                    demand_railway: "КЗХ".into(),
                    from_railways: ["УЗБ"].iter().map(|s| s.to_string()).collect(),
                    surcharge_rub: 0.0,
                },
            ],
            deficit_railways: ["МСК"].iter().map(|s| s.to_string()).collect(),
            deficit_export_max_distance_km: 300,
            deficit_export_surcharge_rub: 30_000.0,
            ..Default::default()
        }
    }

    fn with_railway_s(mut s: SupplyNode, rw: &str) -> SupplyNode {
        s.railway_to = rw.to_string();
        s
    }

    fn with_railway_d(mut d: DemandNode, rw: &str) -> DemandNode {
        d.railway_name = rw.to_string();
        d
    }

    /// Нулевой потолок ГУ-12 не строит дугу с российской дороги и оставляет дугу
    /// с инотерритории. Ненулевой потолок обе дуги строит, инотерриторию помечает
    /// как не расходующую потолок.
    #[test]
    fn gu12_cap_blocks_russian_supply_and_exempts_foreign_origin() {
        let supply = vec![
            with_railway_s(dummy_supply(5, "RU", 1, false), "МСК"),
            with_railway_s(dummy_supply(5, "KZ", 1, false), "КЗХ"),
        ];
        let tariffs = vec![dummy_tariff("RU", "D1"), dummy_tariff("KZ", "D1")];
        let run = |cap: i32| {
            let mut demand = dummy_demand(8, "D1", None);
            demand.gu12_cap = Some(cap);
            demand.railway_name = "ЮВС".into();
            build_task_arcs(
                &supply,
                &[demand],
                &tariffs,
                &HashSet::new(),
                &HashSet::new(),
                &HashMap::new(),
                &rules_roads(),
                &StationBacklogIndex::disabled(),
                &ConventionIndex::disabled(),
            )
        };

        let (arcs, stats) = run(0);
        assert_eq!(stats.gu12_blocked, 1);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].supply_station_code, "KZ");
        assert!(arcs[0].gu12_exempt);

        let (arcs, stats) = run(3);
        assert_eq!(stats.gu12_blocked, 0);
        assert_eq!(arcs.len(), 2);
        let ru = arcs.iter().find(|a| a.supply_station_code == "RU").unwrap();
        let kz = arcs.iter().find(|a| a.supply_station_code == "KZ").unwrap();
        assert!(!ru.gu12_exempt);
        assert!(kz.gu12_exempt);
    }

    /// Потолок расстояния: Load-дуга дальше порога не создаётся и считается в `too_far`,
    /// дуга в пределах порога (включительно) остаётся.
    #[test]
    fn load_arc_beyond_max_distance_dropped() {
        let supply = vec![dummy_supply(3, "S1", 1, false)];
        let demand = vec![dummy_demand(3, "NEAR", None), dummy_demand(3, "FAR", None)];
        let mut near = dummy_tariff("S1", "NEAR");
        near.distance = 5_000; // ровно на пороге — допустимо
        let mut far = dummy_tariff("S1", "FAR");
        far.distance = 5_001;

        let (arcs, stats) = build_task_arcs(
            &supply, &demand, &[near, far],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules_max_km(5_000),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].demand_station_code, "NEAR");
        assert_eq!(stats.too_far, 1);
        assert_eq!(stats.feasible, 1);
    }

    /// Без потолка дальняя дуга сохраняется.
    #[test]
    fn no_max_distance_keeps_far_arc() {
        let supply = vec![dummy_supply(3, "S1", 1, false)];
        let demand = vec![dummy_demand(3, "FAR", None)];
        let mut far = dummy_tariff("S1", "FAR");
        far.distance = 9_000;
        let (arcs, stats) = build_task_arcs(
            &supply, &demand, &[far],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1);
        assert_eq!(stats.too_far, 0);
    }

    /// Правила (потолок, инотерритория, дефицит) не действуют на Wash-дуги:
    /// грязный вагон едет в промывку на любое расстояние и с любой дороги.
    #[test]
    fn rules_do_not_apply_to_wash() {
        let mut s = with_railway_s(dummy_supply(3, "S1", 1, false), "МСК");
        s.prev_etsngs = vec!["421034".to_string()];
        let mut wash_node = with_railway_d(dummy_demand(3, "WASH", None), "КЗХ");
        wash_node.purpose = DemandPurpose::Wash;

        let wash_codes: HashSet<String> = ["421034".to_string()].into_iter().collect();
        let mut wash_tariffs: HashMap<(String, String), TariffNode> = HashMap::new();
        let mut wt = dummy_tariff("S1", "WASH");
        wt.distance = 9_000;
        wash_tariffs.insert(("S1".to_string(), "WASH".to_string()), wt);

        let mut rules = rules_roads();
        rules.max_empty_run_distance_km = Some(1_000);
        let (arcs, stats) = build_task_arcs(
            &[s], &[wash_node], &[],
            &wash_codes, &HashSet::new(), &wash_tariffs,
            &rules,
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1, "wash-дуга не ограничивается бизнес-правилами");
        assert_eq!(stats.too_far, 0);
        assert_eq!(stats.foreign_territory, 0);
        assert_eq!(stats.deficit_export, 0);
        assert!((arcs[0].cost - 1_000.0).abs() < 1e-9, "надбавок правил на wash нет");
    }

    /// `tariff_cost` — чистый тариф для отчёта: у Wash-дуги без надбавки промывочного
    /// маршрута (в `wash_tariffs` она уже включена в cost), у Load-дуги
    /// без надбавок бизнес-правил; `cost` при этом остаётся полной модельной стоимостью.
    #[test]
    fn tariff_cost_excludes_model_surcharges() {
        // Wash: тариф до промывки 7 000 + надбавка 50 000 = 57 000 в wash_tariffs.
        let rules = BusinessRules::default();
        let surcharge = rules.wash_path_surcharge_rub();
        assert_eq!(surcharge, 50_000.0);
        let mut s = dummy_supply(3, "S1", 1, false);
        s.prev_etsngs = vec!["421034".to_string()];
        let mut wash_node = dummy_demand(3, "WASH", None);
        wash_node.purpose = DemandPurpose::Wash;
        let wash_codes: HashSet<String> = ["421034".to_string()].into_iter().collect();
        let mut wash_tariffs: HashMap<(String, String), TariffNode> = HashMap::new();
        let mut wt = dummy_tariff("S1", "WASH");
        wt.cost = 7_000.0 + surcharge;
        wash_tariffs.insert(("S1".to_string(), "WASH".to_string()), wt);

        let (arcs, _) = build_task_arcs(
            &[s], &[wash_node], &[],
            &wash_codes, &HashSet::new(), &wash_tariffs,
            &rules,
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1);
        assert!((arcs[0].cost - (7_000.0 + surcharge)).abs() < 1e-9);
        assert!((arcs[0].tariff_cost - 7_000.0).abs() < 1e-9, "в отчёт — только тариф до промывки");

        // Load с надбавкой бизнес-правила (ОКТ → КЗХ +50 000): tariff_cost = чистый тариф.
        let supply = vec![with_railway_s(dummy_supply(2, "S_OKT", 1, false), "ОКТ")];
        let demand = vec![with_railway_d(dummy_demand(8, "D_KZH", None), "КЗХ")];
        let (arcs, _) = build_task_arcs(
            &supply, &demand, &[dummy_tariff("S_OKT", "D_KZH")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules_roads(),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1);
        assert!((arcs[0].cost - 51_000.0).abs() < 1e-9);
        assert!((arcs[0].tariff_cost - 1_000.0).abs() < 1e-9);
    }

    fn rules_p1_distance(neutral_km: i32, rub_per_km: f64, cap_rub: f64) -> BusinessRules {
        BusinessRules {
            p1_distance_neutral_km: neutral_km,
            p1_distance_rub_per_km: rub_per_km,
            p1_distance_cap_rub: cap_rub,
            ..Default::default()
        }
    }

    /// Поправка периода 1 по расстоянию меняет только `cost`; `tariff_cost` — исходный
    /// тариф. Ближний p1 дешевле p10, дальний p1 дороже p10 с тем же тарифом;
    /// период 10 поправки не получает. Счётчики ArcStats считают бонус/надбавку раздельно.
    #[test]
    fn p1_distance_adjust_shifts_solver_cost_not_report_tariff() {
        let rules = rules_p1_distance(2_000, 5.0, 0.0);
        let mut t_near = dummy_tariff("S1", "D_NEAR");
        t_near.distance = 500; // −7 500
        t_near.cost = 10_000.0;
        let mut t_far = dummy_tariff("S1", "D_FAR");
        t_far.distance = 3_500; // +7 500
        t_far.cost = 10_000.0;
        let p1 = dummy_supply(2, "S1", 1, false);
        let p10 = dummy_supply(2, "S1", 10, false);
        let d_near = dummy_demand(2, "D_NEAR", None);
        let d_far = dummy_demand(2, "D_FAR", None);

        let build = |s: &SupplyNode, d: &DemandNode, t: &TariffNode| {
            build_task_arcs(
                &[s.clone()], &[d.clone()], &[t.clone()],
                &HashSet::new(), &HashSet::new(), &HashMap::new(),
                &rules, &StationBacklogIndex::disabled(), &ConventionIndex::disabled(),
            )
        };
        let (a_p1_near, st_p1_near) = build(&p1, &d_near, &t_near);
        let (a_p10_near, st_p10_near) = build(&p10, &d_near, &t_near);
        let (a_p1_far, st_p1_far) = build(&p1, &d_far, &t_far);
        let (a_p10_far, st_p10_far) = build(&p10, &d_far, &t_far);

        assert_eq!((a_p1_near.len(), a_p10_near.len(), a_p1_far.len(), a_p10_far.len()), (1, 1, 1, 1));
        assert!((a_p1_near[0].cost - 2_500.0).abs() < 1e-9);
        assert!((a_p10_near[0].cost - (10_000.0 + PERIOD10_COST_SURCHARGE_RUB)).abs() < 1e-9);
        assert!(a_p1_near[0].cost < a_p10_near[0].cost);
        assert!((a_p1_far[0].cost - 17_500.0).abs() < 1e-9);
        assert!((a_p10_far[0].cost - (10_000.0 + PERIOD10_COST_SURCHARGE_RUB)).abs() < 1e-9);
        assert!(a_p1_far[0].cost > a_p10_far[0].cost);
        for arc in [&a_p1_near[0], &a_p10_near[0], &a_p1_far[0], &a_p10_far[0]] {
            assert!((arc.tariff_cost - 10_000.0).abs() < 1e-9, "в отчёт — исходный тариф");
        }

        assert_eq!((st_p1_near.arcs_p1_distance_bonus, st_p1_near.arcs_p1_distance_surcharge), (1, 0));
        assert!((st_p1_near.p1_distance_bonus_total_rub - 7_500.0).abs() < 1e-9);
        assert_eq!((st_p1_far.arcs_p1_distance_bonus, st_p1_far.arcs_p1_distance_surcharge), (0, 1));
        assert!((st_p1_far.p1_distance_surcharge_total_rub - 7_500.0).abs() < 1e-9);
        assert_eq!((st_p10_near.arcs_p1_distance_bonus, st_p10_near.arcs_p1_distance_surcharge), (0, 0));
        assert_eq!((st_p10_far.arcs_p1_distance_bonus, st_p10_far.arcs_p1_distance_surcharge), (0, 0));
    }

    /// Потолок поправки ограничивает и бонус, и надбавку; стоимость не уходит ниже нуля.
    #[test]
    fn p1_distance_adjust_capped_and_floored() {
        let rules = rules_p1_distance(2_000, 10.0, 6_000.0);
        let mut t_near = dummy_tariff("S1", "D_NEAR");
        t_near.distance = 0; // −20 000 → −6 000
        t_near.cost = 4_000.0; // 4 000 − 6 000 < 0 → 0
        let mut t_far = dummy_tariff("S1", "D_FAR");
        t_far.distance = 6_000; // +40 000 → +6 000
        t_far.cost = 10_000.0;
        let p1 = dummy_supply(2, "S1", 1, false);

        let (a_near, _) = build_task_arcs(
            &[p1.clone()], &[dummy_demand(2, "D_NEAR", None)], &[t_near],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules, &StationBacklogIndex::disabled(), &ConventionIndex::disabled(),
        );
        let (a_far, _) = build_task_arcs(
            &[p1], &[dummy_demand(2, "D_FAR", None)], &[t_far],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules, &StationBacklogIndex::disabled(), &ConventionIndex::disabled(),
        );
        assert_eq!(a_near.len(), 1);
        assert_eq!(a_far.len(), 1);
        assert!(a_near[0].cost.abs() < 1e-9, "бонус ограничен потолком и стоимость не ниже 0");
        assert!((a_near[0].tariff_cost - 4_000.0).abs() < 1e-9);
        assert!((a_far[0].cost - 16_000.0).abs() < 1e-9);
    }

    /// Поправка не влияет на правило 6: потолок «не дороже промывочного маршрута»
    /// сравнивает реальную стоимость, а не стоимость с надбавкой за дальность.
    /// Прямая погрузка 1 000 против маршрута 1 200 допустима при любой поправке.
    #[test]
    fn p1_distance_adjust_does_not_leak_into_dirty_cap() {
        let mut s = dummy_supply(5, "S1", 1, false);
        s.prev_etsngs = vec!["421034".to_string()];
        let mut d = dummy_demand(5, "D1", None);
        d.etsng = Some("421034".to_string());
        let wash_codes: HashSet<String> = ["421034".to_string()].into_iter().collect();
        let mut wash_tariffs: HashMap<(String, String), TariffNode> = HashMap::new();
        let mut wt = dummy_tariff("S1", "WASH");
        wt.cost = 1_200.0;
        wash_tariffs.insert(("S1".to_string(), "WASH".to_string()), wt);
        let mut t = dummy_tariff("S1", "D1");
        t.distance = 4_000; // далеко: надбавка +10 000 при 5 руб./км от 2 000 км
        t.cost = 1_000.0;

        // С поправкой: прямая погрузка (1 000) ≤ маршрут (1 200) → дуга есть, cost = 11 000.
        let (arcs, stats) = build_task_arcs(
            &[s.clone()], &[d.clone()], &[t.clone()],
            &wash_codes, &HashSet::new(), &wash_tariffs,
            &rules_p1_distance(2_000, 5.0, 0.0),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1, "надбавка за дальность не должна включать потолок правила 6");
        assert_eq!(stats.dirty_far_prefer_wash, 0);
        assert!((arcs[0].cost - 11_000.0).abs() < 1e-9);
        assert!((arcs[0].tariff_cost - 1_000.0).abs() < 1e-9);

        // Контроль: та же пара, но реальная погрузка дороже маршрута — потолок срабатывает
        // независимо от поправки (бонус её не спасает).
        t.cost = 1_300.0;
        t.distance = 0; // бонус −10 000
        let (arcs, stats) = build_task_arcs(
            &[s], &[d], &[t],
            &wash_codes, &HashSet::new(), &wash_tariffs,
            &rules_p1_distance(2_000, 5.0, 0.0),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert!(arcs.is_empty(), "бонус за близость не должен обходить потолок правила 6");
        assert_eq!(stats.dirty_far_prefer_wash, 1);
    }

    /// Правило 1: с российской дороги на инотерриторию дуги нет; ОКТ → КЗХ разрешено
    /// с надбавкой 50 000; УЗБ → КЗХ — без надбавки; КЗХ → КЗХ — свободно.
    #[test]
    fn foreign_territory_rule_on_load_arcs() {
        let supply = vec![
            with_railway_s(dummy_supply(2, "S_GOR", 1, false), "ГОР"),
            with_railway_s(dummy_supply(2, "S_OKT", 1, false), "ОКТ"),
            with_railway_s(dummy_supply(2, "S_UZB", 1, false), "УЗБ"),
            with_railway_s(dummy_supply(2, "S_KZH", 1, false), "КЗХ"),
        ];
        let demand = vec![with_railway_d(dummy_demand(8, "D_KZH", None), "КЗХ")];
        let tariffs: Vec<TariffNode> = supply
            .iter()
            .map(|s| dummy_tariff(&s.station_to_code, "D_KZH"))
            .collect();

        let (arcs, stats) = build_task_arcs(
            &supply, &demand, &tariffs,
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules_roads(),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(stats.foreign_territory, 1, "только ГОР → КЗХ запрещена");
        assert_eq!(arcs.len(), 3);
        assert_eq!(stats.arcs_rule_surcharged, 1);

        let cost_of = |code: &str| arcs.iter().find(|a| a.supply_station_code == code).map(|a| a.cost);
        assert_eq!(cost_of("S_GOR"), None);
        assert_eq!(cost_of("S_OKT"), Some(1_000.0 + 50_000.0));
        assert_eq!(cost_of("S_UZB"), Some(1_000.0));
        assert_eq!(cost_of("S_KZH"), Some(1_000.0));
    }

    /// Правило 2: вывоз с дефицитной МСК на другую дорогу — только ≤ 300 км и с
    /// надбавкой 30 000; длиннее — дуги нет; внутри МСК — без ограничений.
    #[test]
    fn deficit_road_export_rule_on_load_arcs() {
        let supply = vec![with_railway_s(dummy_supply(5, "S_MSK", 1, false), "МСК")];
        let demand = vec![
            with_railway_d(dummy_demand(2, "D_MSK", None), "МСК"),
            with_railway_d(dummy_demand(2, "D_NEAR", None), "ГОР"),
            with_railway_d(dummy_demand(2, "D_FAR", None), "ГОР"),
        ];
        let mut t_msk = dummy_tariff("S_MSK", "D_MSK");
        t_msk.distance = 2_000; // внутри дороги расстояние не ограничено
        let mut t_near = dummy_tariff("S_MSK", "D_NEAR");
        t_near.distance = 300;
        let mut t_far = dummy_tariff("S_MSK", "D_FAR");
        t_far.distance = 301;

        let (arcs, stats) = build_task_arcs(
            &supply, &demand, &[t_msk, t_near, t_far],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules_roads(),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(stats.deficit_export, 1);
        assert_eq!(arcs.len(), 2);
        assert_eq!(stats.arcs_rule_surcharged, 1);

        let cost_of = |code: &str| arcs.iter().find(|a| a.demand_station_code == code).map(|a| a.cost);
        assert_eq!(cost_of("D_MSK"), Some(1_000.0));
        assert_eq!(cost_of("D_NEAR"), Some(1_000.0 + 30_000.0));
        assert_eq!(cost_of("D_FAR"), None);
    }

    /// Средняя станция предложения (= S_MID ваг.) → средне-крупная станция спроса
    /// (= D_MID ваг.): дуги получают порог партии MIN_BATCH_TO_MIDDLE_DEMAND_STATION.
    #[test]
    fn middle_pair_gets_min_batch() {
        let supply = vec![dummy_supply(S_MID, "S1", 1, false)];
        let demand = vec![dummy_demand(D_MID, "D1", None)];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, B_MID);
    }

    /// Периоды 1 и 10 считаются вместе: сумма частей = S_MID → станция средняя.
    #[test]
    fn middle_supply_counts_periods_together() {
        let part10 = S_MID / 2;
        let part1 = S_MID - part10;
        let supply = vec![
            dummy_supply(part1, "S1", 1, false),
            dummy_supply(part10, "S1", 10, false),
        ];
        let demand = vec![dummy_demand(D_MID, "D1", None)];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 2);
        for arc in &arcs {
            assert_eq!(arc.pair_min_batch, B_MID);
        }
    }

    /// Станция предложения S_MID − 1 ваг. → ограничения нет.
    #[test]
    fn small_supply_station_no_min_batch() {
        let supply = vec![dummy_supply(S_MID - 1, "S1", 1, false)];
        let demand = vec![dummy_demand(D_MID, "D1", None)];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, 0);
    }

    /// Станция спроса D_MID − 1 ваг. → ограничения нет.
    #[test]
    fn small_demand_station_no_min_batch() {
        let supply = vec![dummy_supply(S_MID, "S1", 1, false)];
        let demand = vec![dummy_demand(D_MID - 1, "D1", None)];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, 0);
    }

    /// Маршрутная отправка исключается из «среднего» ограничения: на маршрутный узел
    /// действует только маршрутный порог (если станция может собрать партию), но не B_MID.
    #[test]
    fn route_shipping_excluded_from_middle() {
        let supply = vec![dummy_supply(S_MID, "S1", 1, false)];
        let demand = vec![dummy_demand(D_MID, "D1", Some("Маршрутная"))];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, expected_route_batch(S_MID));
    }

    /// Немаршрутный узел на станции, где остальной спрос маршрутный: в сумму станции
    /// входят только немаршрутные узлы (D_MID − 1 < D_MID → «среднего» ограничения нет).
    #[test]
    fn route_nodes_not_counted_in_demand_total() {
        let supply = vec![dummy_supply(S_MID, "S1", 1, false)];
        let demand = vec![
            dummy_demand(D_MID - 1, "D1", None),
            dummy_demand(B_ROUTE + 2, "D1", Some("Маршрутная")),
        ];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 2);
        assert_eq!(arcs[0].pair_min_batch, 0);
        assert_eq!(arcs[1].pair_min_batch, expected_route_batch(S_MID));
    }

    /// Станция массовой выгрузки не считается средней: действует её собственный порог.
    #[test]
    fn mass_station_not_middle() {
        let big = 10 * (S_MID + D_MID + B_ROUTE).max(12);
        let supply = vec![dummy_supply(big, "S1", 1, true)];
        let demand = vec![dummy_demand(D_MID, "D1", None)];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, MIN_BATCH_FROM_MASS_STATION);
    }

    /// Маршрутный узел спроса + станция предложения ≥ B_ROUTE ваг. → маршрутный порог.
    /// Размер маршрутного спроса роли не играет (в example.py route-станции не фильтруются).
    #[test]
    fn route_pair_gets_min_batch() {
        let supply = vec![dummy_supply(B_ROUTE, "S1", 1, false)];
        let demand = vec![dummy_demand(B_ROUTE + 2, "D1", Some("Маршрутная"))];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, B_ROUTE);
    }

    /// Станция предложения B_ROUTE − 1 ваг. не может собрать маршрутную партию —
    /// её дуги на маршрутные узлы без ограничения (example.py: s_route_stations_filtered).
    #[test]
    fn route_pair_small_supply_station_no_constraint() {
        let supply = vec![dummy_supply(B_ROUTE - 1, "S1", 1, false)];
        let demand = vec![dummy_demand(B_ROUTE + 2, "D1", Some("Маршрутная"))];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, 0);
    }

    /// Периоды 1 и 10 считаются вместе и для маршрутного порога: сумма частей = B_ROUTE.
    #[test]
    fn route_supply_counts_periods_together() {
        let part10 = B_ROUTE / 2;
        let part1 = B_ROUTE - part10;
        let supply = vec![
            dummy_supply(part1, "S1", 1, false),
            dummy_supply(part10, "S1", 10, false),
        ];
        let demand = vec![dummy_demand(B_ROUTE + 5, "D1", Some("Маршрутная"))];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 2);
        for arc in &arcs {
            assert_eq!(arc.pair_min_batch, B_ROUTE);
        }
    }

    /// Массовая станция предложения на маршрутный узел: действует маршрутный порог
    /// (в example.py route-секция не исключает массовые станции).
    #[test]
    fn mass_supply_to_route_demand_gets_route_batch() {
        let big = 10 * (S_MID + D_MID + B_ROUTE).max(12);
        let supply = vec![dummy_supply(big, "S1", 1, true)];
        let demand = vec![dummy_demand(B_ROUTE + 5, "D1", Some("Маршрутная"))];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, B_ROUTE);
    }

    /// Маршрутные и немаршрутные узлы одной станции погрузки — разные группы
    /// с разными порогами (B_ROUTE и B_MID), pair_key их различает.
    #[test]
    fn mixed_route_and_regular_demand_on_same_station() {
        // Предложение покрывает оба порога отбора: и маршрутный, и «средний».
        let supply = vec![dummy_supply(B_ROUTE.max(S_MID) + 2, "S1", 1, false)];
        let demand = vec![
            dummy_demand(B_ROUTE, "D1", Some("Маршрутная")),
            dummy_demand(D_MID, "D1", None),
        ];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 2);
        assert_eq!(arcs[0].pair_min_batch, B_ROUTE);
        assert_eq!(arcs[1].pair_min_batch, B_MID);
        if B_ROUTE != B_MID {
            assert_ne!(arcs[0].pair_key(), arcs[1].pair_key());
        }
    }

    /// collect_pair_min_batch_violations ловит поток 0 < total < B на средней паре.
    #[test]
    fn violations_detected_for_middle_pair() {
        if B_MID <= 1 {
            return; // при пороге ≤ 1 нарушение 0 < x < B невозможно
        }
        let supply = vec![dummy_supply(S_MID, "S1", 1, false)];
        let demand = vec![dummy_demand(D_MID, "D1", None)];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);

        let v = collect_pair_min_batch_violations([(0_usize, B_MID - 1)].into_iter(), &arcs);
        assert_eq!(v, vec![("S1".to_string(), "D1".to_string(), B_MID)]);

        let ok = collect_pair_min_batch_violations([(0_usize, B_MID)].into_iter(), &arcs);
        assert!(ok.is_empty());
    }

    /// Грязный вагон + аналогичный груз, но прямая погрузка (1000) дороже
    /// промывочного маршрута (500): дуга прямой погрузки не создаётся,
    /// пара относится к причине DirtyFarLoadPreferWash.
    #[test]
    fn dirty_far_load_dropped_when_wash_cheaper() {
        let mut s = dummy_supply(5, "S1", 1, false);
        s.prev_etsngs = vec!["421034".to_string()];
        let mut d = dummy_demand(5, "D1", None);
        d.etsng = Some("421034".to_string());

        let wash_codes: HashSet<String> = ["421034".to_string()].into_iter().collect();
        let mut wash_tariffs: HashMap<(String, String), TariffNode> = HashMap::new();
        let mut wt = dummy_tariff("S1", "WASH");
        wt.cost = 500.0; // промывочный маршрут дешевле прямой погрузки (1000)
        wash_tariffs.insert(("S1".to_string(), "WASH".to_string()), wt);

        let (arcs, stats) = build_task_arcs(
            &[s], &[d], &[dummy_tariff("S1", "D1")],
            &wash_codes, &HashSet::new(), &wash_tariffs,
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert!(arcs.is_empty(), "дальняя погрузка дороже промывки — дуги быть не должно");
        assert_eq!(stats.dirty_far_prefer_wash, 1);
        assert_eq!(stats.dirty_etsng_mismatch, 0);
    }

    /// Грязный вагон + аналогичный груз, прямая погрузка (1000) дешевле
    /// промывочного маршрута (2000): дуга прямой погрузки сохраняется.
    #[test]
    fn dirty_load_kept_when_direct_cheaper() {
        let mut s = dummy_supply(5, "S1", 1, false);
        s.prev_etsngs = vec!["421034".to_string()];
        let mut d = dummy_demand(5, "D1", None);
        d.etsng = Some("421034".to_string());

        let wash_codes: HashSet<String> = ["421034".to_string()].into_iter().collect();
        let mut wash_tariffs: HashMap<(String, String), TariffNode> = HashMap::new();
        let mut wt = dummy_tariff("S1", "WASH");
        wt.cost = 2000.0; // промывочный маршрут дороже прямой погрузки (1000)
        wash_tariffs.insert(("S1".to_string(), "WASH".to_string()), wt);

        let (arcs, stats) = build_task_arcs(
            &[s], &[d], &[dummy_tariff("S1", "D1")],
            &wash_codes, &HashSet::new(), &wash_tariffs,
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1, "прямая погрузка дешевле промывки — дуга должна остаться");
        assert_eq!(stats.dirty_far_prefer_wash, 0);
    }

    /// Грязный вагон без доступной промывки (нет wash-тарифа): cap не применяется,
    /// прямая погрузка под аналогичный груз сохраняется даже если она дорогая.
    #[test]
    fn dirty_load_kept_when_no_wash_available() {
        let mut s = dummy_supply(5, "S1", 1, false);
        s.prev_etsngs = vec!["421034".to_string()];
        let mut d = dummy_demand(5, "D1", None);
        d.etsng = Some("421034".to_string());

        let wash_codes: HashSet<String> = ["421034".to_string()].into_iter().collect();
        let wash_tariffs: HashMap<(String, String), TariffNode> = HashMap::new();

        let (arcs, stats) = build_task_arcs(
            &[s], &[d], &[dummy_tariff("S1", "D1")],
            &wash_codes, &HashSet::new(), &wash_tariffs,
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1);
        assert_eq!(stats.dirty_far_prefer_wash, 0);
    }

    /// Правило 6, коэффициент потолка: при `k = 1.5` прямая погрузка за 1 000 допустима
    /// против промывочного маршрута 800 (потолок 1 200); при `k ≤ 0` потолка нет вовсе.
    #[test]
    fn dirty_far_load_cap_ratio_from_rules() {
        let mut s = dummy_supply(5, "S1", 1, false);
        s.prev_etsngs = vec!["421034".to_string()];
        let mut d = dummy_demand(5, "D1", None);
        d.etsng = Some("421034".to_string());
        let wash_codes: HashSet<String> = ["421034".to_string()].into_iter().collect();
        let mut wash_tariffs: HashMap<(String, String), TariffNode> = HashMap::new();
        let mut wt = dummy_tariff("S1", "WASH");
        wt.cost = 800.0;
        wash_tariffs.insert(("S1".to_string(), "WASH".to_string()), wt);

        let run = |ratio: f64| {
            let rules = BusinessRules { dirty_same_cargo_max_cost_ratio_to_wash: ratio, ..Default::default() };
            build_task_arcs(
                &[s.clone()], &[d.clone()], &[dummy_tariff("S1", "D1")],
                &wash_codes, &HashSet::new(), &wash_tariffs,
                &rules,
                &StationBacklogIndex::disabled(),
                &ConventionIndex::disabled(),
            )
        };
        let (arcs, stats) = run(1.0);
        assert!(arcs.is_empty(), "k = 1: 1 000 > 800 — в промывку");
        assert_eq!(stats.dirty_far_prefer_wash, 1);
        let (arcs, stats) = run(1.5);
        assert_eq!(arcs.len(), 1, "k = 1.5: 1 000 ≤ 1 200 — дуга есть");
        assert_eq!(stats.dirty_far_prefer_wash, 0);
        let (arcs, _) = run(0.0);
        assert_eq!(arcs.len(), 1, "k ≤ 0: потолок выключен");
    }

    /// Правило 6, поощрение: у дуги «грязный → тот же ЕТСНГ» стоимость снижается на
    /// `p × (тариф до ближайшей промывки + промывка)`; чистый вагон на ту же заявку и
    /// Wash-дуга поощрения не получают; потолок считается по стоимости до поощрения;
    /// `tariff_cost` остаётся чистым тарифом.
    #[test]
    fn dirty_same_cargo_reward_lowers_only_dirty_load_arc() {
        let rules = BusinessRules {
            wash_procedure_cost_rub: 10_000.0,
            empty_run_after_wash_cost_rub: 40_000.0,
            dirty_same_cargo_reward_share: 0.5,
            ..Default::default()
        };
        let mut dirty = dummy_supply(5, "S1", 1, false);
        dirty.prev_etsngs = vec!["421034".to_string()];
        let clean = dummy_supply(5, "S2", 1, false);
        let mut d = dummy_demand(5, "D1", None);
        d.etsng = Some("421034".to_string());
        let mut wash_node = dummy_demand(5, "WASH", None);
        wash_node.purpose = DemandPurpose::Wash;
        let wash_codes: HashSet<String> = ["421034".to_string()].into_iter().collect();

        // Промывка в 20 000 от S1: маршрут 70 000; отложенная промывка 20 000 + 10 000 = 30 000.
        let mut wash_tariffs: HashMap<(String, String), TariffNode> = HashMap::new();
        let mut wt = dummy_tariff("S1", "WASH");
        wt.cost = 20_000.0 + rules.wash_path_surcharge_rub();
        wash_tariffs.insert(("S1".to_string(), "WASH".to_string()), wt);

        // Прямая погрузка грязного 60 000 (≤ 70 000 — в потолке), чистого — 50 000.
        let mut t_dirty = dummy_tariff("S1", "D1");
        t_dirty.cost = 60_000.0;
        let mut t_clean = dummy_tariff("S2", "D1");
        t_clean.cost = 50_000.0;

        let (arcs, stats) = build_task_arcs(
            &[dirty, clean], &[d, wash_node], &[t_dirty, t_clean],
            &wash_codes, &HashSet::new(), &wash_tariffs,
            &rules,
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 3, "грязный → D1, грязный → WASH, чистый → D1");
        let arc = |s_idx: usize, d_idx: usize| arcs.iter().find(|a| a.s_idx == s_idx && a.d_idx == d_idx).unwrap();
        let dirty_load = arc(0, 0);
        assert!((dirty_load.cost - 45_000.0).abs() < 1e-6, "60 000 − 0.5 × 30 000, получено {}", dirty_load.cost);
        assert!((dirty_load.tariff_cost - 60_000.0).abs() < 1e-6, "в отчёт — тариф без поощрения");
        assert!((arc(0, 1).cost - 70_000.0).abs() < 1e-6, "Wash-дуга без поощрения");
        assert!((arc(1, 0).cost - 50_000.0).abs() < 1e-6, "чистый вагон без поощрения");
        assert_eq!(stats.arcs_dirty_rewarded, 1);
        assert!((stats.dirty_reward_total_rub - 15_000.0).abs() < 1e-6);
        // Грязный вагон теперь дешевле чистого на ту же заявку — заявку получит он.
        assert!(dirty_load.cost < arc(1, 0).cost);
    }

    /// Правило 6, поощрение без промывочного маршрута: базой служит средняя надбавка
    /// целиком; стоимость дуги не опускается ниже нуля.
    #[test]
    fn dirty_same_cargo_reward_without_wash_route_is_floored_at_zero() {
        let rules = BusinessRules { dirty_same_cargo_reward_share: 1.0, ..Default::default() };
        let mut s = dummy_supply(5, "S1", 1, false);
        s.prev_etsngs = vec!["421034".to_string()];
        let mut d = dummy_demand(5, "D1", None);
        d.etsng = Some("421034".to_string());
        let wash_codes: HashSet<String> = ["421034".to_string()].into_iter().collect();
        // Тариф 1 000 < поощрение 1 × 50 000 → стоимость 0, поощрение учтено в размере тарифа.
        let (arcs, stats) = build_task_arcs(
            &[s], &[d], &[dummy_tariff("S1", "D1")],
            &wash_codes, &HashSet::new(), &HashMap::new(),
            &rules,
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1);
        assert!(arcs[0].cost.abs() < 1e-9, "не ниже нуля, получено {}", arcs[0].cost);
        assert!((arcs[0].tariff_cost - 1_000.0).abs() < 1e-9);
        assert_eq!(stats.arcs_dirty_rewarded, 1);
        assert!((stats.dirty_reward_total_rub - 1_000.0).abs() < 1e-9);
    }

    // -----------------------------------------------------------------------
    // Правило 8: иномойка и капризные дороги
    // -----------------------------------------------------------------------

    fn rules_foreign_washed() -> BusinessRules {
        BusinessRules {
            foreign_washed_roads: ["КЗХ".into()].into_iter().collect(),
            foreign_washed_picky_railways: ["МСК".into()].into_iter().collect(),
            foreign_washed_picky_surcharge_rub: 30_000.0,
            ..Default::default()
        }
    }

    /// Вагон с иномойки и «грязным» ЕТСНГ идёт под любой груз и не едет в промывку.
    #[test]
    fn foreign_washed_wagon_is_not_dirty() {
        let mut s = with_railway_s(dummy_supply(5, "S1", 1, false), "КЗХ");
        s.prev_etsngs = vec!["421034".to_string()];
        let mut load = with_railway_d(dummy_demand(5, "D1", None), "ЮВС");
        load.etsng = Some("999999".to_string()); // другой ЕТСНГ — для грязного был бы запрет
        let mut wash_node = dummy_demand(5, "WASH", None);
        wash_node.purpose = DemandPurpose::Wash;

        let wash_codes: HashSet<String> = ["421034".to_string()].into_iter().collect();
        let mut wash_tariffs: HashMap<(String, String), TariffNode> = HashMap::new();
        wash_tariffs.insert(("S1".to_string(), "WASH".to_string()), dummy_tariff("S1", "WASH"));

        let (arcs, stats) = build_task_arcs(
            &[s], &[load, wash_node], &[dummy_tariff("S1", "D1")],
            &wash_codes, &HashSet::new(), &wash_tariffs,
            &rules_foreign_washed(),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(stats.dirty_etsng_mismatch, 0);
        assert_eq!(arcs.len(), 1, "только погрузка, без промывки");
        assert_eq!(arcs[0].demand_station_code, "D1");
    }

    /// При профиците подсыл иномойки на МСК дороже, чем на другую дорогу, на величину надбавки.
    #[test]
    fn foreign_washed_picky_surcharge_on_surplus() {
        // 10 ваг. предложения > 1.2 × (3+3) спроса → профицит, надбавка на МСК.
        let s = with_railway_s(dummy_supply(10, "S1", 1, false), "КЗХ");
        let d_msk = with_railway_d(dummy_demand(3, "D_MSK", None), "МСК");
        let d_yvs = with_railway_d(dummy_demand(3, "D_YVS", None), "ЮВС");
        let (arcs, stats) = build_task_arcs(
            &[s], &[d_msk, d_yvs],
            &[dummy_tariff("S1", "D_MSK"), dummy_tariff("S1", "D_YVS")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules_foreign_washed(),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 2);
        assert_eq!(stats.arcs_foreign_washed_picky, 1);
        let cost = |code: &str| arcs.iter().find(|a| a.demand_station_code == code).map(|a| a.cost);
        assert_eq!(cost("D_MSK"), Some(31_000.0));
        assert_eq!(cost("D_YVS"), Some(1_000.0));
    }

    /// При дефиците капризная надбавка не применяется — вагоны нужны.
    #[test]
    fn foreign_washed_picky_off_when_deficit() {
        let s = with_railway_s(dummy_supply(2, "S1", 1, false), "КЗХ");
        let d_msk = with_railway_d(dummy_demand(10, "D_MSK", None), "МСК");
        let (arcs, stats) = build_task_arcs(
            &[s], &[d_msk], &[dummy_tariff("S1", "D_MSK")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules_foreign_washed(),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1);
        assert_eq!(stats.arcs_foreign_washed_picky, 0);
        assert!((arcs[0].cost - 1_000.0).abs() < 1e-9);
    }

    /// Небольшой перевес предложения (в пределах запаса 1.2) — ещё не профицит:
    /// 7 ваг. против 6 заявок надбавку не включает, 8 — включает.
    #[test]
    fn foreign_washed_picky_needs_surplus_margin() {
        let d_msk = with_railway_d(dummy_demand(6, "D_MSK", None), "МСК");
        let build = |cars: i32| {
            let s = with_railway_s(dummy_supply(cars, "S1", 1, false), "КЗХ");
            build_task_arcs(
                &[s], &[d_msk.clone()], &[dummy_tariff("S1", "D_MSK")],
                &HashSet::new(), &HashSet::new(), &HashMap::new(),
                &rules_foreign_washed(),
                &StationBacklogIndex::disabled(),
                &ConventionIndex::disabled(),
            )
        };
        let (arcs, stats) = build(7); // 7 > 7.2 — нет
        assert_eq!(stats.arcs_foreign_washed_picky, 0);
        assert!((arcs[0].cost - 1_000.0).abs() < 1e-9);
        let (arcs, stats) = build(8); // 8 > 7.2 — профицит
        assert_eq!(stats.arcs_foreign_washed_picky, 1);
        assert!((arcs[0].cost - 31_000.0).abs() < 1e-9);
    }

    /// Российский вагон на МСК при профиците надбавки иномойки не получает.
    #[test]
    fn picky_surcharge_skips_russian_supply() {
        let s = with_railway_s(dummy_supply(10, "S1", 1, false), "СКВ");
        let d_msk = with_railway_d(dummy_demand(3, "D_MSK", None), "МСК");
        let (arcs, stats) = build_task_arcs(
            &[s], &[d_msk], &[dummy_tariff("S1", "D_MSK")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules_foreign_washed(),
            &StationBacklogIndex::disabled(),
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1);
        assert_eq!(stats.arcs_foreign_washed_picky, 0);
        assert!((arcs[0].cost - 1_000.0).abs() < 1e-9);
    }

    // -----------------------------------------------------------------------
    // Правило 4: загруженность станции погрузки
    // -----------------------------------------------------------------------

    /// Правила с включённым правилом 4: K_hard, K_soft, штраф ожидания 5 000 ₽/сут.
    fn rules_backlog(hard: i32, soft: i32) -> BusinessRules {
        BusinessRules {
            station_backlog_hard_days: Some(hard),
            station_backlog_soft_days: soft,
            station_backlog_wait_penalty_rub_per_day: 5_000.0,
            ..Default::default()
        }
    }

    /// Индекс загруженности по мощности `capacity` станции `code` и Q из узлов спроса.
    fn backlog_index(code: &str, capacity: i32, demand: &[DemandNode], rules: &BusinessRules) -> StationBacklogIndex {
        let caps: HashMap<String, i32> = [(code.to_string(), capacity)].into_iter().collect();
        StationBacklogIndex::build(&caps, demand, rules)
    }

    fn with_q(mut d: DemandNode, q: i32) -> DemandNode {
        d.cars_on_station = q;
        d
    }

    fn with_period(mut d: DemandNode, period: u8) -> DemandNode {
        d.period = period;
        d
    }

    /// Жёсткая часть: Q ≥ K_hard·C — станция закрыта во все периоды, дуг нет,
    /// пары считаются в `station_overloaded`. Ниже K_hard·C — открыта.
    #[test]
    fn overloaded_station_closed_for_all_periods() {
        let rules = rules_backlog(5, 1);
        let supply = vec![dummy_supply(5, "S1", 1, false), dummy_supply(5, "S2", 10, false)];
        // C = 10, Q = 50 ≥ 50 → закрыта; спрос в периодах 1 и 4.
        let demand = vec![
            with_q(with_period(dummy_demand(5, "D1", None), 1), 50),
            with_q(with_period(dummy_demand(5, "D1", None), 4), 50),
        ];
        let idx = backlog_index("D1", 10, &demand, &rules);
        let mut far = dummy_tariff("S2", "D1");
        far.period_of_delivery = 6;
        let (arcs, stats) = build_task_arcs(
            &supply, &demand, &[dummy_tariff("S1", "D1"), far],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules, &idx,
            &ConventionIndex::disabled(),
        );
        assert!(arcs.is_empty(), "закрытая станция: дуг нет даже в период 4 и от дислокации");
        assert_eq!(stats.station_overloaded, 4);
        assert_eq!(stats.feasible, 0);

        // Q = 49 — ниже порога: открыта.
        let demand_ok = vec![with_q(dummy_demand(5, "D1", None), 49)];
        let idx_ok = backlog_index("D1", 10, &demand_ok, &rules);
        let (arcs, stats) = build_task_arcs(
            &supply[..1], &demand_ok, &[dummy_tariff("S1", "D1")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules, &idx_ok,
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1);
        assert_eq!(stats.station_overloaded, 0);
    }

    /// Мягкая часть: очередь сдвигает ожидаемые сутки погрузки. C = 10, Q = 41,
    /// K_soft = 1 → t* = ceil(31/10) = 4. Срок подсыла 1 сут. → ожидание 3 сут.:
    /// погрузка на 4-е сутки — в окне периода 1 (0–4), штрафа за срок нет,
    /// к тарифу добавлено 3 × 5 000 ₽ ожидания.
    #[test]
    fn backlog_wait_adds_penalty_and_keeps_window() {
        let rules = rules_backlog(5, 1);
        let s = dummy_supply(5, "S1", 1, false);
        let demand = vec![with_q(dummy_demand(5, "D1", None), 41)];
        let idx = backlog_index("D1", 10, &demand, &rules);
        assert_eq!(idx.get("D1").unwrap().backlog_clear_day, 4);

        let (arcs, stats) = build_task_arcs(
            &[s], &demand, &[dummy_tariff("S1", "D1")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules, &idx,
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1);
        let a = &arcs[0];
        assert!(a.period_ok, "погрузка на 4-е сутки укладывается в период 1");
        assert!((a.cost - (1_000.0 + 3.0 * 5_000.0)).abs() < 1e-6, "cost = тариф + 3 сут. ожидания");
        assert!((a.tariff_cost - 1_000.0).abs() < 1e-6, "чистый тариф без штрафов");
        assert_eq!(stats.arcs_backlog_wait, 1);
        assert_eq!(stats.arcs_period_penalized, 0);
    }

    /// Ожидание выталкивает погрузку за окно периода: K_hard = 11, K_soft = 0,
    /// C = 10, Q = 100 → t* = 10. Срок подсыла 1 сут. → ожидание 9, погрузка на 10-е
    /// сутки: для периода 1 (окно до 7) нарушение 3 сут. → штраф 3 × 15 000 плюс
    /// 9 × 5 000 ожидания; для периода 4 (10–14) — без нарушения, только ожидание.
    #[test]
    fn backlog_wait_can_violate_window_of_early_period() {
        let rules = rules_backlog(11, 0);
        let s = dummy_supply(5, "S1", 1, false);
        let demand = vec![
            with_q(with_period(dummy_demand(5, "D1", None), 1), 100),
            with_q(with_period(dummy_demand(5, "D1", None), 4), 100),
        ];
        let idx = backlog_index("D1", 10, &demand, &rules);
        assert_eq!(idx.get("D1").unwrap().backlog_clear_day, 10);

        let (arcs, stats) = build_task_arcs(
            &[s], &demand, &[dummy_tariff("S1", "D1")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules, &idx,
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 2);
        let p1 = arcs.iter().find(|a| a.d_idx == 0).unwrap();
        let p4 = arcs.iter().find(|a| a.d_idx == 1).unwrap();
        assert!(!p1.period_ok);
        let expected_p1 = 1_000.0 + 3.0 * PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB + 9.0 * 5_000.0;
        assert!((p1.cost - expected_p1).abs() < 1e-6, "p1 cost {} != {expected_p1}", p1.cost);
        assert!(p4.period_ok);
        assert!((p4.cost - (1_000.0 + 9.0 * 5_000.0)).abs() < 1e-6);
        assert_eq!(stats.arcs_backlog_wait, 2);
        assert_eq!(stats.arcs_period_penalized, 1);
    }

    /// Вагон дислокации освобождается на 5 суток позже: прибытие 5 + 1 = 6 ≥ t* = 4 —
    /// ожидания нет, стоимость как без правила 4 (тариф + надбавка периода 10).
    #[test]
    fn dislocation_supply_arrives_after_backlog_clears() {
        let rules = rules_backlog(5, 1);
        let s = dummy_supply(5, "S1", 10, false);
        let demand = vec![with_q(with_period(dummy_demand(5, "D1", None), 2), 41)];
        let idx = backlog_index("D1", 10, &demand, &rules);

        let (arcs, stats) = build_task_arcs(
            &[s], &demand, &[dummy_tariff("S1", "D1")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules, &idx,
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1);
        assert!((arcs[0].cost - (1_000.0 + PERIOD10_COST_SURCHARGE_RUB)).abs() < 1e-6);
        assert_eq!(stats.arcs_backlog_wait, 0);
    }

    /// Станция без мощности в справочнике (или C = 0) правилом не проверяется,
    /// как и узлы промывки; отключённое правило (индекс пуст) ничего не меняет.
    #[test]
    fn backlog_rule_skips_unknown_capacity_and_wash() {
        let rules = rules_backlog(5, 1);
        let s = dummy_supply(5, "S1", 1, false);
        let demand = vec![with_q(dummy_demand(5, "D1", None), 999)];
        // Мощность известна только у другой станции.
        let idx = backlog_index("OTHER", 10, &demand, &rules);
        let (arcs, stats) = build_task_arcs(
            &[s], &demand, &[dummy_tariff("S1", "D1")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &rules, &idx,
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1);
        assert_eq!(stats.station_overloaded, 0);
        assert!((arcs[0].cost - 1_000.0).abs() < 1e-6);

        // Промывка: Q огромное, но Wash-узлы под правило не попадают.
        let mut dirty = dummy_supply(5, "S1", 1, false);
        dirty.prev_etsngs = vec!["421034".to_string()];
        let mut wash_node = with_q(dummy_demand(5, "WASH", None), 999);
        wash_node.purpose = DemandPurpose::Wash;
        let wash_codes: HashSet<String> = ["421034".to_string()].into_iter().collect();
        let mut wash_tariffs: HashMap<(String, String), TariffNode> = HashMap::new();
        wash_tariffs.insert(("S1".to_string(), "WASH".to_string()), dummy_tariff("S1", "WASH"));
        let idx_wash = backlog_index("WASH", 10, &[wash_node.clone()], &rules);
        assert!(idx_wash.get("WASH").is_none(), "Wash-узлы в индекс не входят");
        let (arcs, stats) = build_task_arcs(
            &[dirty], &[wash_node], &[],
            &wash_codes, &HashSet::new(), &wash_tariffs,
            &rules, &idx_wash,
            &ConventionIndex::disabled(),
        );
        assert_eq!(arcs.len(), 1);
        assert_eq!(stats.station_overloaded, 0);
    }

    fn conv_rule_empty_esr(rzd: &str, esr: &str) -> crate::data::conventions::ParsedConvention {
        crate::data::conventions::ParsedConvention {
            rzd_number: rzd.into(),
            cargo_class: crate::data::conventions::ConventionCargoClass::Empty,
            cargo_name: String::new(),
            date_beg: "2026-01-01".into(),
            date_end: "3000-01-01".into(),
            dest_esr: vec![esr.into()],
            dest_names: vec![],
            dest_railways: vec![],
            dest_all_stations: false,
            dep_esr: vec![],
            dep_names: vec![],
            dep_railways: vec![],
            dep_all_stations: false,
            junction: None,
            all_parties: true,
            recipient_okpo: vec![],
            recipient_names: vec![],
            unknown_road_fragments: vec![],
            convention_info: crate::data::conventions::ConventionStatus::Other,
            kzh_stripped_dest: false,
            kzh_stripped_dep: false,
        }
    }

    #[test]
    fn convention_empty_esr_drops_load_arc() {
        let idx = ConventionIndex::build(vec![conv_rule_empty_esr("9001", "D1")]);
        let supply = vec![dummy_supply(3, "S1", 1, false)];
        let demand = vec![dummy_demand(3, "D1", None), dummy_demand(3, "D2", None)];
        let (arcs, stats) = build_task_arcs(
            &supply, &demand, &[dummy_tariff("S1", "D1"), dummy_tariff("S1", "D2")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &idx,
        );
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].demand_station_code, "D2");
        assert_eq!(stats.convention_ban, 1);
        assert_eq!(stats.convention_by_number.get("9001"), Some(&1));
    }

    #[test]
    fn convention_4702_style_closes_only_dep_to_dest_pair() {
        let mut r = conv_rule_empty_esr("4702", "x");
        r.dest_esr.clear();
        r.cargo_class = crate::data::conventions::ConventionCargoClass::All;
        r.dep_railways = vec!["ЗСБ".into()];
        r.dest_railways = vec!["ОКТ".into()];
        r.dest_all_stations = true;
        r.dep_all_stations = true;
        let idx = ConventionIndex::build(vec![r]);
        let s = with_railway_s(dummy_supply(3, "S1", 1, false), "ЗСБ");
        let mut zsb_okt = with_railway_d(dummy_demand(3, "D1", None), "ЗСБ");
        zsb_okt.railway_to_name = Some("ОКТ".into());
        let mut zsb_skv = with_railway_d(dummy_demand(3, "D2", None), "ЗСБ");
        zsb_skv.railway_to_name = Some("СКВ".into());
        let mut msk_okt = with_railway_d(dummy_demand(3, "D3", None), "МСК");
        msk_okt.railway_to_name = Some("ОКТ".into());
        let (arcs, stats) = build_task_arcs(
            &[s],
            &[zsb_okt, zsb_skv, msk_okt],
            &[dummy_tariff("S1", "D1"), dummy_tariff("S1", "D2"), dummy_tariff("S1", "D3")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &idx,
        );
        let dests: Vec<&str> = arcs.iter().map(|a| a.demand_station_code.as_str()).collect();
        assert!(!dests.contains(&"D1"));
        assert!(dests.contains(&"D2"));
        assert!(dests.contains(&"D3"));
        assert_eq!(stats.convention_ban, 1);
    }

    #[test]
    fn convention_wash_telegram_drops_wash_not_load() {
        let mut r = conv_rule_empty_esr("W1", "WASH");
        r.convention_info = crate::data::conventions::ConventionStatus::WashingStation;
        let idx = ConventionIndex::build(vec![r]);
        let mut s = dummy_supply(3, "S1", 1, false);
        s.prev_etsngs = vec!["421034".into()];
        let mut wash = dummy_demand(3, "WASH", None);
        wash.purpose = DemandPurpose::Wash;
        let mut load = dummy_demand(3, "WASH", None);
        load.etsng = Some("421034".into());
        let wash_codes: HashSet<String> = ["421034".into()].into_iter().collect();
        let mut wash_tariffs = HashMap::new();
        wash_tariffs.insert(("S1".into(), "WASH".into()), dummy_tariff("S1", "WASH"));
        let (arcs, stats) = build_task_arcs(
            &[s.clone()], &[wash, load], &[dummy_tariff("S1", "WASH")],
            &wash_codes, &HashSet::new(), &wash_tariffs,
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &idx,
        );
        assert_eq!(stats.convention_ban, 1);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].d_idx, 1, "осталась дуга погрузки, промывка закрыта конвенцией");
    }

    #[test]
    fn convention_empty_esr_closes_only_matching_sender_okpo() {
        let mut r = conv_rule_empty_esr("P1", "D1");
        r.all_parties = false;
        r.recipient_okpo = vec![crate::data::gu12::normalize_okpo("00111")];
        let idx = ConventionIndex::build(vec![r]);
        let s = dummy_supply(3, "S1", 1, false);
        let mut hit = dummy_demand(3, "D1", None);
        hit.sender_okpo = Some("00111".into());
        let mut miss = dummy_demand(3, "D1", None);
        miss.sender_okpo = Some("00222".into());
        let (arcs, stats) = build_task_arcs(
            &[s], &[hit, miss], &[dummy_tariff("S1", "D1")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &idx,
        );
        assert_eq!(stats.convention_ban, 1);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].d_idx, 1);
    }

    #[test]
    fn convention_grain_closes_cargo_dest_not_load_station() {
        let mut r = conv_rule_empty_esr("G1", "514003");
        r.cargo_class = crate::data::conventions::ConventionCargoClass::Grain;
        let idx = ConventionIndex::build(vec![r]);
        let s = dummy_supply(3, "S1", 1, false);
        let mut closed = dummy_demand(3, "LOAD1", None);
        closed.station_to_code = Some("514003".into());
        let mut open = dummy_demand(3, "LOAD2", None);
        open.station_to_code = Some("200002".into());
        let (arcs, stats) = build_task_arcs(
            &[s], &[closed, open],
            &[dummy_tariff("S1", "LOAD1"), dummy_tariff("S1", "LOAD2")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &idx,
        );
        let dests: Vec<&str> = arcs.iter().map(|a| a.demand_station_code.as_str()).collect();
        assert!(!dests.contains(&"LOAD1"));
        assert!(dests.contains(&"LOAD2"));
        assert_eq!(stats.convention_ban, 1);
    }

    #[test]
    fn convention_expired_on_arrival_does_not_ban() {
        let mut r = conv_rule_empty_esr("E1", "D1");
        r.date_beg = "2026-01-01".into();
        r.date_end = "2026-09-01".into();
        let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 15).unwrap();
        let idx = ConventionIndex::build_at(vec![r], today);
        let s = dummy_supply(3, "S1", 1, false);
        let d = dummy_demand(3, "D1", None);
        let (arcs, stats) = build_task_arcs(
            &[s], &[d], &[dummy_tariff("S1", "D1")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &idx,
        );
        assert_eq!(arcs.len(), 1);
        assert_eq!(stats.convention_ban, 0);
    }

    #[test]
    fn convention_empty_ban_on_dispatch_day_closes_arc_even_if_arrival_later() {
        // Телеграмма действует только сегодня; порожний отправляется сегодня (period 1),
        // прибывает через сутки — запрет приёма к отправлению всё равно действует.
        let mut r = conv_rule_empty_esr("E2", "D1");
        r.date_beg = "2026-09-15".into();
        r.date_end = "2026-09-15".into();
        let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 15).unwrap();
        let idx = ConventionIndex::build_at(vec![r], today);
        let d = dummy_demand(3, "D1", None);
        let (arcs, stats) = build_task_arcs(
            &[dummy_supply(3, "S1", 1, false)], &[d.clone()], &[dummy_tariff("S1", "D1")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &idx,
        );
        assert_eq!(arcs.len(), 0);
        assert_eq!(stats.convention_ban, 1);

        // Дислокация (supply_period 10): отправление через 5 суток — телеграмма уже истекла.
        let (arcs, stats) = build_task_arcs(
            &[dummy_supply(3, "S1", 10, false)], &[with_period(d, 2)], &[dummy_tariff("S1", "D1")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &idx,
        );
        assert_eq!(arcs.len(), 1);
        assert_eq!(stats.convention_ban, 0);
    }

    #[test]
    fn convention_grain_ban_checks_loading_window_of_demand_period() {
        // Grain-запрет 25…29 сентября. Порожний отправляется и прибывает раньше, но узел
        // периода 4 грузится на 10–14-е сутки (25…29.09) — дуга закрыта; узел периода 1 — открыт.
        let mut r = conv_rule_empty_esr("G2", "514003");
        r.cargo_class = crate::data::conventions::ConventionCargoClass::Grain;
        r.date_beg = "2026-09-25".into();
        r.date_end = "2026-09-29".into();
        let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 15).unwrap();
        let idx = ConventionIndex::build_at(vec![r], today);
        let mut late = with_period(dummy_demand(3, "LOAD1", None), 4);
        late.station_to_code = Some("514003".into());
        let mut early = dummy_demand(3, "LOAD2", None);
        early.station_to_code = Some("514003".into());
        let (arcs, stats) = build_task_arcs(
            &[dummy_supply(6, "S1", 1, false)], &[late, early],
            &[dummy_tariff("S1", "LOAD1"), dummy_tariff("S1", "LOAD2")],
            &HashSet::new(), &HashSet::new(), &HashMap::new(),
            &BusinessRules::default(),
            &StationBacklogIndex::disabled(),
            &idx,
        );
        let dests: Vec<&str> = arcs.iter().map(|a| a.demand_station_code.as_str()).collect();
        assert!(!dests.contains(&"LOAD1"), "период 4 грузится в окно телеграммы");
        assert!(dests.contains(&"LOAD2"), "период 1 грузится до начала телеграммы");
        assert_eq!(stats.convention_ban, 1);
    }
}

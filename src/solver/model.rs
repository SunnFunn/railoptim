use std::collections::{HashMap, HashSet};

use crate::data::references::normalize_etsng_code;
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
pub const MIN_BATCH_FROM_MASS_STATION: i32 = 3;

/// Минимальный размер партии «средняя станция предложения → средне-крупная станция погрузки».
///
/// Бизнес-логика: подсыл по 1–2 вагона невыгоден клиенту — маневровые работы на станции
/// оплачиваются за подачу, а не за вагон. Аналог `_ASSIGN_LOW_BOUND_` из example.py.
///
/// Поток по паре станций должен удовлетворять: `x == 0 || x >= MIN_BATCH_TO_MIDDLE_DEMAND_STATION`.
pub const MIN_BATCH_TO_MIDDLE_DEMAND_STATION: i32 = 2;

/// Минимальное суммарное предложение на станции образования (все периоды),
/// при котором станция считается **средней** и попадает под ограничение
/// [`MIN_BATCH_TO_MIDDLE_DEMAND_STATION`]. Станции массовой выгрузки исключаются
/// (для них действует [`MIN_BATCH_FROM_MASS_STATION`]). Аналог `_SUPPLY_SIZE_BOUND_`.
pub const MIDDLE_SUPPLY_STATION_MIN_CARS: i32 = 7;

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
/// Вдвое выше [`PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB`], что отражает бо́льшую
/// неопределённость в сроках порожних из дислокации. Окно при этом сдвигается на −5 сут.:
/// проверяется `[L − 3 − 5, U + 3 − 5]`.
pub const PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_PERIOD10_RUB: f64 = 30_000.0;

/// Надбавка к стоимости дуг предложения с `supply_period == 10` (дислокация 2–10 суток).
///
/// Делает вагоны дислокации менее привлекательными для решателя по сравнению
/// с вагонами периода 1 (готовы сегодня). Если оба вагона могут закрыть один
/// узел спроса и разница в тарифе ≤ `PERIOD10_COST_SURCHARGE_RUB`, решатель
/// предпочтёт вагон периода 1.
///
/// [`super::lp::PENALTY_UNMET`] — period=10 остаётся конкурентным там, где
/// period=1 объективно недоступен (нет тарифа, нарушение срока).
pub const PERIOD10_COST_SURCHARGE_RUB: f64 = 120_000.0;

/// Средняя стоимость промывки вагона (руб.), добавляется к тарифу «до станции промывки»
/// для честного сравнения с назначением под погрузку аналогичного груза.
pub const WASH_PROCEDURE_AVG_COST_RUB: f64 = 10_000.0;

/// Средняя стоимость порожнего пробега после промывки до погрузки (руб.), добавляется к тарифу до промывки.
pub const EMPTY_RUN_AFTER_WASH_TO_LOAD_AVG_COST_RUB: f64 = 40_000.0;

/// Полная надбавка к тарифу до станции промывки для оптимизации.
pub const WASH_PATH_SURCHARGE_RUB: f64 =
    WASH_PROCEDURE_AVG_COST_RUB + EMPTY_RUN_AFTER_WASH_TO_LOAD_AVG_COST_RUB;

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

    /// Стоимость перевозки, руб.
    pub cost: f64,
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
/// период 1 — сумма `Normativ` за сутки 1–3, период 10 — сумма за сутки 4–6.
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
///   [`PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_PERIOD10_RUB`]/сут. (вдвое выше).
///
/// [`TaskArc::period_ok`] == `true` означает, что нарушения окна нет.
/// Неудовлетворённый спрос обрабатывается slack-переменными в [`super::lp::solve`].
///
/// Возвращает `(arcs, stats)`, где `stats` — счётчики для диагностики.
///
/// `tariffs` — тарифы до станций **погрузки** (как из АПИ).
/// `wash_tariffs` — тарифы до станций **промывки** с уже учтённой надбавкой
/// [`WASH_PATH_SURCHARGE_RUB`] (промывка + порожний пробег до погрузки), ключ `(откуда, куда)`.
pub fn build_task_arcs(
    supply: &[SupplyNode],
    demand: &[DemandNode],
    tariffs: &[TariffNode],
    wash_codes: &HashSet<String>,
    no_cleaning_roads: &HashSet<String>,
    wash_tariffs: &HashMap<(String, String), TariffNode>,
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
    let mut arcs_period_penalized = 0usize;

    for (s_idx, s) in supply.iter().enumerate() {
        for (d_idx, d) in demand.iter().enumerate() {
            let tariff: &TariffNode = match d.purpose {
                DemandPurpose::Wash => {
                    // Вагоны с дорогой образования из NoCleaningRoads — не грязные
                    // (промывка уже оплачена клиентом на иностранной территории).
                    if !supply_needs_wash(s, wash_codes, no_cleaning_roads) {
                        no_tariff += 1;
                        continue;
                    }
                    let key = (s.station_to_code.clone(), d.station_code.clone());
                    let Some(t) = wash_tariffs.get(&key) else {
                        no_tariff += 1;
                        continue;
                    };
                    t
                }
                DemandPurpose::Load => {
                    // Ограничение «грязного» вагона:
                    // если вагон из-под груза, требующего промывки (и не освобождён
                    // по NoCleaningRoads), он может быть назначен под погрузку
                    // ТОЛЬКО под тот же ЕТСНГ.
                    // Альтернативный маршрут — через узел промывки (DemandPurpose::Wash).
                    if supply_needs_wash(s, wash_codes, no_cleaning_roads) {
                        let supply_etsng = effective_etsng_for_wash_tariff(s);
                        let demand_etsng = d.etsng.as_deref().map(normalize_etsng_code);
                        match (supply_etsng, demand_etsng) {
                            (Some(se), Some(de)) if se == de => {} // ETSNG совпадает → дуга разрешена
                            _ => {
                                dirty_etsng_mismatch += 1;
                                continue;
                            }
                        }
                    }

                    let key = (s.station_to_code.as_str(), d.station_code.as_str());
                    let Some(t) = tariff_index.get(&key) else {
                        no_tariff += 1;
                        continue;
                    };
                    *t
                }
            };

            let car_type_ok = car_type_compatible(s.car_type.as_deref(), d.car_type.as_deref());
            if !car_type_ok {
                bad_type += 1;
                continue;
            }

            let (period_ok, cost) = {
                let penalty_rate = if s.supply_period == 10 {
                    PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_PERIOD10_RUB
                } else {
                    PER_DAY_DELIVERY_PERIOD_VIOLATION_PENALTY_RUB
                };
                let Some(violation_days) = delivery_window_violation_days(
                    tariff.period_of_delivery,
                    d.period,
                    s.supply_period,
                ) else {
                    bad_period += 1;
                    continue;
                };
                let period_ok = violation_days == 0;
                if violation_days > 0 {
                    arcs_period_penalized += 1;
                }
                let penalty = violation_days as f64 * penalty_rate;
                (period_ok, tariff.cost + penalty)
            };

            // надбавка к стоимости дуг period=10 для приоритизации period=1.
            let cost = if s.supply_period == 10 {
                cost + PERIOD10_COST_SURCHARGE_RUB
            } else {
                cost
            };

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

            arcs.push(TaskArc {
                arc_id: arcs.len(),
                s_idx,
                d_idx,
                supply_station_code: s.station_to_code.clone(),
                demand_station_code: d.station_code.clone(),
                cost,
                distance:          tariff.distance,
                delivery_days:     tariff.period_of_delivery,
                period_ok,
                car_type_ok:       true,
                pair_min_batch,
            });
        }
    }

    let stats = ArcStats {
        total_pairs: supply.len() * demand.len(),
        no_tariff,
        bad_period,
        bad_type,
        dirty_etsng_mismatch,
        feasible: arcs.len(),
        arcs_period_penalized,
    };

    (arcs, stats)
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
    /// Пар «грязный» вагон → погрузка с несовпадающим ЕТСНГ (запрещено без промывки).
    pub dirty_etsng_mismatch: usize,
    /// Допустимых дуг (вошли в LP).
    pub feasible:   usize,
    /// Дуг с ненулевым штрафом за срок подсыла (`supply_period != 10`, вне `[L−3, U+3]`).
    pub arcs_period_penalized: usize,
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
fn demand_period_day_bounds(period: u8) -> Option<(i32, i32)> {
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
    let shift    = if supply_period == 10 { 5 } else { 0 };
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
        );
        arcs
    }

    /// Средняя станция предложения (7 ваг.) → средне-крупная станция спроса (5 ваг.):
    /// дуги получают порог партии MIN_BATCH_TO_MIDDLE_DEMAND_STATION.
    #[test]
    fn middle_pair_gets_min_batch() {
        let supply = vec![dummy_supply(7, "S1", 1, false)];
        let demand = vec![dummy_demand(5, "D1", None)];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, MIN_BATCH_TO_MIDDLE_DEMAND_STATION);
    }

    /// Периоды 1 и 10 считаются вместе: 4 + 3 = 7 ваг. → станция средняя.
    #[test]
    fn middle_supply_counts_periods_together() {
        let supply = vec![
            dummy_supply(4, "S1", 1, false),
            dummy_supply(3, "S1", 10, false),
        ];
        let demand = vec![dummy_demand(5, "D1", None)];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 2);
        for arc in &arcs {
            assert_eq!(arc.pair_min_batch, MIN_BATCH_TO_MIDDLE_DEMAND_STATION);
        }
    }

    /// Станция предложения 6 ваг. (< 7) → ограничения нет.
    #[test]
    fn small_supply_station_no_min_batch() {
        let supply = vec![dummy_supply(6, "S1", 1, false)];
        let demand = vec![dummy_demand(5, "D1", None)];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, 0);
    }

    /// Станция спроса 4 ваг. (< 5) → ограничения нет.
    #[test]
    fn small_demand_station_no_min_batch() {
        let supply = vec![dummy_supply(7, "S1", 1, false)];
        let demand = vec![dummy_demand(4, "D1", None)];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, 0);
    }

    /// Маршрутная отправка исключается: и из суммы станции, и из ограничения дуги.
    #[test]
    fn route_shipping_excluded_from_middle() {
        let supply = vec![dummy_supply(7, "S1", 1, false)];
        let demand = vec![dummy_demand(5, "D1", Some("Маршрутная"))];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, 0);
    }

    /// Немаршрутный узел на станции, где остальной спрос маршрутный: в сумму станции
    /// входят только немаршрутные узлы (4 < 5 → ограничения нет).
    #[test]
    fn route_nodes_not_counted_in_demand_total() {
        let supply = vec![dummy_supply(7, "S1", 1, false)];
        let demand = vec![
            dummy_demand(4, "D1", None),
            dummy_demand(10, "D1", Some("Маршрутная")),
        ];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 2);
        for arc in &arcs {
            assert_eq!(arc.pair_min_batch, 0);
        }
    }

    /// Станция массовой выгрузки не считается средней: действует её собственный порог.
    #[test]
    fn mass_station_not_middle() {
        let supply = vec![dummy_supply(120, "S1", 1, true)];
        let demand = vec![dummy_demand(5, "D1", None)];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, MIN_BATCH_FROM_MASS_STATION);
    }

    /// Маршрутный узел спроса + станция предложения ≥ 10 ваг. → порог партии 10.
    /// Размер маршрутного спроса роли не играет (в example.py route-станции не фильтруются).
    #[test]
    fn route_pair_gets_min_batch() {
        let supply = vec![dummy_supply(10, "S1", 1, false)];
        let demand = vec![dummy_demand(12, "D1", Some("Маршрутная"))];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, MIN_BATCH_TO_ROUTE_DEMAND_STATION);
    }

    /// Станция предложения 9 ваг. (< 10) не может собрать маршрутную партию —
    /// её дуги на маршрутные узлы без ограничения (example.py: s_route_stations_filtered).
    #[test]
    fn route_pair_small_supply_station_no_constraint() {
        let supply = vec![dummy_supply(9, "S1", 1, false)];
        let demand = vec![dummy_demand(12, "D1", Some("Маршрутная"))];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, 0);
    }

    /// Периоды 1 и 10 считаются вместе и для маршрутного порога: 6 + 4 = 10 ваг.
    #[test]
    fn route_supply_counts_periods_together() {
        let supply = vec![
            dummy_supply(6, "S1", 1, false),
            dummy_supply(4, "S1", 10, false),
        ];
        let demand = vec![dummy_demand(15, "D1", Some("Маршрутная"))];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 2);
        for arc in &arcs {
            assert_eq!(arc.pair_min_batch, MIN_BATCH_TO_ROUTE_DEMAND_STATION);
        }
    }

    /// Массовая станция предложения на маршрутный узел: действует маршрутный порог
    /// (10 строже 3; в example.py route-секция не исключает массовые станции).
    #[test]
    fn mass_supply_to_route_demand_gets_route_batch() {
        let supply = vec![dummy_supply(120, "S1", 1, true)];
        let demand = vec![dummy_demand(15, "D1", Some("Маршрутная"))];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].pair_min_batch, MIN_BATCH_TO_ROUTE_DEMAND_STATION);
    }

    /// Маршрутные и немаршрутные узлы одной станции погрузки — разные группы
    /// с разными порогами (10 и 3), pair_key их различает.
    #[test]
    fn mixed_route_and_regular_demand_on_same_station() {
        let supply = vec![dummy_supply(12, "S1", 1, false)];
        let demand = vec![
            dummy_demand(10, "D1", Some("Маршрутная")),
            dummy_demand(5, "D1", None),
        ];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);
        assert_eq!(arcs.len(), 2);
        assert_eq!(arcs[0].pair_min_batch, MIN_BATCH_TO_ROUTE_DEMAND_STATION);
        assert_eq!(arcs[1].pair_min_batch, MIN_BATCH_TO_MIDDLE_DEMAND_STATION);
        assert_ne!(arcs[0].pair_key(), arcs[1].pair_key());
    }

    /// collect_pair_min_batch_violations ловит поток 0 < total < B на средней паре.
    #[test]
    fn violations_detected_for_middle_pair() {
        let supply = vec![dummy_supply(7, "S1", 1, false)];
        let demand = vec![dummy_demand(5, "D1", None)];
        let arcs = build(&supply, &demand, &[dummy_tariff("S1", "D1")]);

        let v = collect_pair_min_batch_violations([(0_usize, 2_i32)].into_iter(), &arcs);
        assert_eq!(
            v,
            vec![("S1".to_string(), "D1".to_string(), MIN_BATCH_TO_MIDDLE_DEMAND_STATION)]
        );

        let ok = collect_pair_min_batch_violations([(0_usize, 3_i32)].into_iter(), &arcs);
        assert!(ok.is_empty());
    }
}

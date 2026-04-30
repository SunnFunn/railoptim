use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

/// Назначение узла спроса: погрузка порожних или приём «грязных» на промывку.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemandPurpose {
    /// Спрос на погрузку (исходные данные АПИ).
    #[default]
    Load,
    /// Спрос на промывку (ёмкость станций промывки).
    Wash,
}

/// Группа вагона в узле предложения.
#[derive(Debug, Clone, PartialEq)]
pub enum CarKind {
    /// С номером, свободен (OPZRailWayId == null).
    Free,
    /// С номером, уже назначен и идёт по факту (OPZRailWayId != null).
    Assigned,
    /// Безномерной (из opzNoNumberModelCollection).
    NoNumber,
}

/// Статус вагона по ремонту.
#[derive(Debug, Clone, PartialEq)]
pub enum RepairStatus {
    /// Вагон не требует ремонта в горизонте планирования.
    Ok,
    /// Вагон подлежит ремонту: IsCarRepair=true или days_to_repair < 15.
    NeedsRepair,
}

/// Сгруппированный узел предложения порожних вагонов.
///
/// Вагоны сгруппированы по однородным признакам: станция назначения, тип, ЕТСНГ,
/// статус ремонта. Внутри группы индивидуальные данные собраны в списки.
#[derive(Debug, Clone)]
pub struct SupplyNode {
    /// Уникальный ID узла, присваивается при группировке.
    pub s_id: usize,
    /// Группа вагонов: свободные / по факту / безномерные.
    pub kind: CarKind,
    /// Количество вагонов в группе.
    pub car_count: i32,

    // --- Ключ группировки ---
    pub station_to:      String,
    pub station_to_code: String,
    pub railway_to:      String,
    pub railway_to_code: Option<i32>,
    pub railway_part_to: Option<String>,
    /// Тип вагона (например, "БКТ", "Прочие", "Т"). None для безномерных.
    pub car_type:        Option<String>,
    pub etsng:           Option<String>,
    pub etsng_name:      Option<String>,
    /// Статус ремонта группы.
    pub repair_status:   RepairStatus,
    /// "ГРУЖ" или "ПОР" (GRPOName). None для безномерных.
    pub status:          Option<String>,

    /// `1` — предложение из АПИ (1-е сутки); `10` — дислокация Redis+MSSQL (2–10 сутки).
    pub supply_period: u8,

    // --- Агрегированные: номера вагонов (пусто для NoNumber) ---
    pub car_numbers: Vec<u64>,

    // --- Агрегированные: станции/дороги отправления (по одной на каждый вагон) ---
    pub stations_from:      Vec<String>,
    pub stations_from_code: Vec<String>,
    pub railways_from:      Vec<String>,
    pub railways_from_code: Vec<i32>,
    pub railways_part_from: Vec<String>,

    /// `true` — станция назначения является станцией массовой выгрузки:
    /// суммарное количество вагонов по всем узлам с этой станцией > порога.
    /// Заполняется после группировки в [`crate::data::supply`].
    pub is_mass_unloading: bool,

    // --- Агрегированные: состояние груза ---
    pub prev_etsngs:      Vec<String>,
    pub prev_etsng_names: Vec<String>,

}


#[derive(Deserialize, Debug, Clone)]
pub struct DemandNode {
    /// Уникальный ID узла спроса, присваивается при конвертации из API-ответа.
    #[serde(default)]
    pub d_id: usize,

    /// Погрузка или промывка (для узлов из АПИ всегда [`DemandPurpose::Load`]).
    #[serde(default)]
    pub purpose: DemandPurpose,

    /// Номер планового периода погрузки: 1 (сут. 1–5), 2 (6–8), 3 (9–10), 4 (11–15).
    pub period: u8,

    // --- Станция и дорога погрузки (From) ---
    pub station_name: String,
    pub station_code: String,
    pub railway_name: String,
    pub railway_code: Option<String>,
    /// Отделение дороги погрузки (RailWayPartFrom).
    pub railway_part: Option<String>,

    // --- Станция и дорога назначения (To) ---
    pub station_to_name: Option<String>,
    pub station_to_code: Option<String>,
    pub railway_to_name: Option<String>,
    pub railway_to_code: Option<String>,
    pub railway_to_part: Option<String>,

    // --- Грузоотправитель ---
    pub sender: Option<String>,
    pub sender_okpo: Option<String>,
    /// Код ТГНЛ грузоотправителя.
    pub sender_tgnl: Option<String>,

    // --- Клиент и грузополучатель ---
    pub client: Option<Vec<String>>,
    pub customer_okpo: Option<Vec<String>>,
    pub recipient: Option<Vec<String>>,
    pub loader_to_okpo: Option<Vec<String>>,

    // --- Груз ---
    pub gng_cargo: Option<String>,
    pub etsng: Option<String>,

    // --- Заявки ---
    pub request_numbers: Option<Vec<String>>,
    pub request_dates: Option<Vec<String>>,
    pub gu12_number: Option<Vec<String>>,

    // --- Параметры вагонов ---
    pub shipping_type: Option<String>,
    /// "БКТ" если вес > 70 т/ваг, иначе "Прочие".
    pub car_type: Option<String>,
    /// Потребность в вагонах = PlannedCarsToLoad − ProvidedCarsToLoad (≥ 0).
    pub car_count: i32,
    /// Количество вагонов на станции для оценки ее загруженности.
    pub cars_on_station: i32,
}

/// Тарифный узел: стоимость, расстояние и срок доставки для пары станций.
///
/// Формируется из ответа `GetRailTariffRouteDataTransmission`.
/// Используется как ребро графа «предложение → спрос» в оптимизационной задаче.
#[derive(Debug, Clone)]
pub struct TariffNode {
    // --- Станция отправления ---
    pub station_from:      String,
    pub station_from_code: String,
    pub railway_from:      String,
    pub railway_from_code: i32,

    // --- Станция назначения ---
    pub station_to:      String,
    pub station_to_code: String,
    pub railway_to:      String,
    pub railway_to_code: i32,

    // --- Тариф ---
    /// Расстояние в километрах.
    pub distance: i32,
    /// Нормативный срок доставки, сутки.
    pub period_of_delivery: i32,
    /// Стоимость перевозки, руб.
    pub cost: f64,
    /// Дата актуальности тарифа.
    pub actual_date: NaiveDateTime,
}

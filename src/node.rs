use chrono::NaiveDateTime;
use serde::Deserialize;


#[derive(Debug, PartialEq, Clone, Ord, PartialOrd, Eq)]
pub struct Node {
    pub s_node_id: usize,
    pub s_station_code: Option<String>,
    pub s_qty: i32,
    pub d_node_id: usize,
    pub d_station_code: Option<String>,
    pub d_qty: i32,
    pub node_cost: i32,
    pub node_qty: i32,
}

impl Node {
    pub fn new_with_data(
        s_id: usize,
        s_station_code: Option<String>,
        d_id: usize,
        d_station_code: Option<String>,
        s_qt: i32,
        d_qt: i32,
        cost: i32,
    ) -> Node {
        Node {
            s_node_id: s_id,
            s_station_code,
            s_qty: s_qt,
            d_node_id: d_id,
            d_station_code,
            d_qty: d_qt,
            node_cost: cost,
            node_qty: 0,
        }
    }
}


/// Группа вагона в узле предложения.
#[derive(Debug, Clone, PartialEq)]
pub enum CarKind {
    /// С номером, свободен (OPZRailWayId == null).
    Free,
    /// С номером, уже назначен и идёт по факту (OPZRailWayId != null).
    Assigned,
    /// Безномерной (из opzNoNumberModelCollection); данных по вагону нет.
    NoNumber,
}

/// Узел предложения порожних вагонов.
#[derive(Debug, Clone)]
pub struct SupplyNode {
    /// Уникальный ID узла, присваивается при конвертации.
    pub s_id: usize,

    /// Группа вагона: свободный / по факту / безномерной.
    pub kind: CarKind,

    // --- Идентификатор ---
    /// Номер вагона. None для безномерных.
    pub car_number: Option<u64>,
    /// Количество вагонов в узле (1 для именных, N для безномерных).
    pub car_count: i32,

    // --- Станция и дорога отправления (только именные) ---
    pub station_from: Option<String>,
    pub station_from_code: Option<String>,
    pub railway_from: Option<String>,
    pub railway_from_code: Option<i32>,
    pub railway_part_from: Option<String>,

    // --- Станция и дорога назначения ---
    pub station_to: String,
    pub station_to_code: String,
    pub railway_to: String,
    pub railway_to_code: Option<i32>,
    pub railway_part_to: Option<String>,

    // --- Характеристики вагона (только именные) ---
    pub capacity: f64,
    pub volume: f64,
    /// Тип вагона из OPZComment1 (например, "БКТ", "Прочие").
    pub car_type: Option<String>,
    pub car_model: Option<String>,

    // --- Состояние груза (только именные) ---
    /// "ГРУЖ" или "ПОР" (GRPOName).
    pub status: Option<String>,
    pub etsng: Option<String>,
    pub etsng_name: Option<String>,
    pub prev_etsng: Option<String>,
    pub prev_etsng_name: Option<String>,

    // --- Ремонт (только именные) ---
    pub days_to_repair: Option<f64>,
    pub repair_type: Option<String>,

    // --- Комментарии (только именные) ---
    pub comment_odo: Option<String>,
    pub comment_odo2: Option<String>,
    /// Непустые OPZComment1..10, объединённые через " | ".
    pub opz_comments: Option<String>,

    pub next_claim: Option<String>,
    pub idle_time: Option<f64>,
}


#[derive(Deserialize, Debug)]
pub struct DemandNode {
    /// Уникальный ID узла спроса, присваивается при конвертации из API-ответа.
    #[serde(default)]
    pub d_id: usize,

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
    /// Количество вагонов на станции дляоценки ее загруженности.
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
    pub cost: i64,
    /// Дата актуальности тарифа.
    pub actual_date: NaiveDateTime,
}

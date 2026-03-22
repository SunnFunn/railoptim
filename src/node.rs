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


#[derive(Deserialize, Debug)]
pub struct SupplyNode {
    #[serde(rename = "sNodeID")]
    pub s_id: usize,

    #[serde(default, rename = "Номера вагонов")]
    pub car_number: Option<Vec<String>>,

    #[serde(rename = "Количество вагонов")]
    pub car_count: i32,

    #[serde(rename = "Период")]
    pub period: Option<i32>,

    #[serde(default, rename = "ГРУЖ/ПОР")]
    pub status: String,

    #[serde(default, rename = "Грузоподъемность вагона")]
    pub capacity: f64,

    #[serde(rename = "Кубатура вагона")]
    pub volume: f64,

    #[serde(default, rename = "Габарит вагона")]
    pub gauge: String,

    #[serde(default, rename = "Груз")]
    pub cargo: Option<String>,

    #[serde(rename = "ЕТСНГ")]
    pub etsng: Option<String>,

    #[serde(default, rename = "Предыдущий груз")]
    pub prev_cargo: Option<String>,

    #[serde(default, rename = "Предыдущий ЕТСНГ")]
    pub prev_etsng: Option<String>,

    #[serde(default, rename = "Станция отправления")]
    pub station_from: String,

    #[serde(default, rename = "Код станции отправления")]
    pub station_from_code: String, // Используем String для сохранения нулей

    #[serde(default, rename = "Дорога отправления")]
    pub railway_from: String,

    #[serde(default, rename = "Код дороги отправления")]
    pub railway_from_code: Option<i32>,

    #[serde(default, rename = "Отделение дороги отправления")]
    pub r_division_from: Option<String>,

    #[serde(default, rename = "Станция назначения")]
    pub station_to: String,

    #[serde(rename = "Код станции назначения")]
    pub station_to_code: String,

    #[serde(default, rename = "Дорога назначения")]
    pub railway_to: String,

    #[serde(rename = "Код дороги назначения")]
    pub railway_to_code: Option<i32>,

    #[serde(default, rename = "Отделение дороги назначения")]
    pub r_division_to: Option<String>,

    #[serde(default, rename = "Признак массовой выгрузки")]
    pub mass_unloading: i32,

    #[serde(default, rename = "Грузополучатель")]
    pub recipient: String,

    #[serde(default, rename = "Расстояние до станции назначения")]
    pub distance_to_dest: Option<f64>,

    #[serde(default, rename = "Назначение")]
    pub assignment: Option<String>,

    #[serde(default, rename = "Следующая заявка")]
    pub next_claim: Option<String>,

    #[serde(default, rename = "Статус ремонта")]
    pub repair_status: i32,

    #[serde(default, rename = "Дней до ремонта")]
    pub days_to_repair: Option<i32>,

    #[serde(default, rename = "Комментарии")]
    pub comments: Option<String>,
}


#[derive(Deserialize, Debug)]
pub struct DemandNode {
    #[serde(rename = "dNodeID")]
    pub d_id: usize,

    #[serde(default, rename = "Станция погрузки")]
    pub station_name: String,

    #[serde(rename = "Код станции погрузки")]
    pub station_code: String, // Код ЕСР

    #[serde(default, rename = "Дорога погрузки")]
    pub railway_name: String,

    #[serde(default, rename = "Код дороги погрузки")]
    pub railway_code: Option<i32>,

    #[serde(default, rename = "Отделение дороги погрузки")]
    pub division: Option<String>,

    #[serde(default, rename = "Номера заявок")]
    pub request_numbers: Option<Vec<String>>,

    #[serde(default, rename = "Даты заявок")]
    pub request_dates: Option<Vec<String>>,

    #[serde(default, rename = "№ ГУ-12")]
    pub gu12_number: Option<Vec<String>>,

    #[serde(default, rename = "Клиент")]
    pub client: Option<String>,

    #[serde(default, rename = "Грузоотправитель")]
    pub sender: Option<String>,

    #[serde(default, rename = "Грузоотправитель ОКПО")]
    pub sender_okpo: Option<String>,

    #[serde(default, rename = "Грузополучатель")]
    pub recipient: Option<String>,

    #[serde(default, rename = "Груз ГНГ")]
    pub gng_cargo: Option<String>,

    #[serde(rename = "ЕТСНГ")]
    pub etsng: Option<String>,

    #[serde(rename = "Тип отправки")]
    pub shipping_type: Option<String>,

    #[serde(rename = "Тип вагона")]
    pub car_type: Option<String>,

    #[serde(rename = "Период")]
    pub period: Option<String>,

    #[serde(rename = "Количество вагонов")]
    pub car_count: i32,
}

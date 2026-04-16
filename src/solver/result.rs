use std::fs;
use std::path::PathBuf;

use chrono::Local;
use serde::Serialize;

use std::collections::{BTreeMap, HashMap};

use crate::node::{DemandNode, SupplyNode, CarKind, TariffNode};
use crate::data::repairs::RepairStation;
use super::lp::OptimResult;
use super::model::TaskArc;

// ---------------------------------------------------------------------------
// Структуры отчёта
// ---------------------------------------------------------------------------

/// Одна строка плана назначения: конкретный вагон (или группа) → узел спроса.
#[derive(Serialize, Debug)]
pub struct AssignmentRecord {
    /// Назначено вагонов.
    pub cars: f64,

    // --- Предложение ---
    pub supply_id:           usize,
    pub supply_kind:         &'static str,
    /// Номера вагонов в группе (пусто для NoNumber).
    pub car_numbers:         Vec<u64>,
    pub supply_station:      String,
    pub supply_station_code: String,
    pub supply_railway:      String,

    // --- Спрос ---
    pub demand_id:           usize,
    pub demand_station:      String,
    pub demand_station_code: String,
    pub demand_railway:      String,
    pub demand_period:       u8,

    // --- Тариф ---
    pub cost_rub:      f64,
    pub distance_km:   i32,
    pub delivery_days: i32,
    pub period_ok:     bool,
    pub car_type_ok:   bool,
}

/// Полный отчёт об одном прогоне оптимизации.
#[derive(Serialize, Debug)]
pub struct OptimReport {
    pub timestamp:       String,
    pub solver_status:   String,
    pub total_cost_rub:  f64,
    pub assigned_cars:   f64,
    pub penalty_cars:    f64,
    pub supply_count:    usize,
    pub demand_count:    usize,
    pub arc_count:       usize,
    pub assignments:     Vec<AssignmentRecord>,
}

// ---------------------------------------------------------------------------
// Построение отчёта
// ---------------------------------------------------------------------------

/// Строит полный отчёт из результата LP-решателя.
pub fn build_report(
    result:  &OptimResult,
    solution: &[f64],
    arcs:    &[TaskArc],
    supply:  &[SupplyNode],
    demand:  &[DemandNode],
) -> OptimReport {
    let assignments = arcs
        .iter()
        .zip(solution.iter())
        .filter(|(_, qty)| **qty > 1e-4)
        .map(|(arc, &cars)| {
            let s = &supply[arc.s_idx];
            let d = &demand[arc.d_idx];
            AssignmentRecord {
                cars,
                supply_id:           s.s_id,
                supply_kind:         car_kind_str(&s.kind),
                car_numbers:         s.car_numbers.clone(),
                supply_station:      s.station_to.clone(),
                supply_station_code: arc.supply_station_code.clone(),
                supply_railway:      s.railway_to.clone(),
                demand_id:           d.d_id,
                demand_station:      d.station_name.clone(),
                demand_station_code: arc.demand_station_code.clone(),
                demand_railway:      d.railway_name.clone(),
                demand_period:       d.period,
                cost_rub:            arc.cost,
                distance_km:         arc.distance,
                delivery_days:       arc.delivery_days,
                period_ok:           arc.period_ok,
                car_type_ok:         arc.car_type_ok,
            }
        })
        .collect();

    OptimReport {
        timestamp:      Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        solver_status:  result.status.clone(),
        total_cost_rub: result.total_cost,
        assigned_cars:  result.assigned_cars,
        penalty_cars:   result.penalty_cars,
        supply_count:   supply.len(),
        demand_count:   demand.len(),
        arc_count:      arcs.len(),
        assignments,
    }
}

// ---------------------------------------------------------------------------
// Сохранение на диск
// ---------------------------------------------------------------------------

/// Сохраняет отчёт в `tmp/result_YYYYMMDD_HHMMSS.json`.
///
/// Директория `tmp/` создаётся автоматически при отсутствии.
pub fn save_result(report: &OptimReport) -> anyhow::Result<PathBuf> {
    let dir = PathBuf::from("tmp");
    fs::create_dir_all(&dir)?;

    let filename = format!(
        "result_{}.json",
        Local::now().format("%Y%m%d_%H%M%S")
    );
    let path = dir.join(filename);

    let json = serde_json::to_string_pretty(report)?;
    fs::write(&path, json)?;

    Ok(path)
}

// ---------------------------------------------------------------------------
// Вспомогательное
// ---------------------------------------------------------------------------

fn car_kind_str(kind: &CarKind) -> &'static str {
    match kind {
        CarKind::Free     => "Free",
        CarKind::Assigned => "Assigned",
        CarKind::NoNumber => "NoNumber",
    }
}

pub fn period_range_str(period: u8) -> &'static str {
    match period {
        1 => "1-5",
        2 => "6-8",
        3 => "9-10",
        4 => "11-15",
        _ => "?",
    }
}

// ---------------------------------------------------------------------------
// Выходные данные для АПИ (схема request.json / DestinationRegistryTransmission)
// ---------------------------------------------------------------------------

/// Одна запись плана назначения в формате API.
///
/// Поля `supply_kind` и `period_label` помечены `#[serde(skip)]`
/// — они не отправляются в API, но используются для листа Excel.
#[derive(Serialize, Debug, Clone)]
pub struct OutputRecord {
    #[serde(rename = "OPZDate")]
    pub opz_date: String,

    // --- Откуда (узел предложения) ---
    #[serde(rename = "RailWayFrom")]
    pub railway_from: String,
    #[serde(rename = "RailWayFromDivision")]
    pub railway_from_div: Option<String>,
    #[serde(rename = "StationFrom")]
    pub station_from: String,
    #[serde(rename = "StationFromCode")]
    pub station_from_code: String,

    // --- Куда (узел спроса) ---
    #[serde(rename = "RailWayTo")]
    pub railway_to: String,
    #[serde(rename = "RailWayToDivision")]
    pub railway_to_div: Option<String>,
    #[serde(rename = "StationTo")]
    pub station_to: String,
    #[serde(rename = "StationToCode")]
    pub station_to_code: String,

    // --- Назначение ---
    #[serde(rename = "AssignedCarsAmount")]
    pub assigned_cars: i32,
    #[serde(rename = "LoadStatus")]
    pub load_status: Option<String>,
    #[serde(rename = "CarType")]
    pub car_type: Option<String>,

    // --- Груз ---
    #[serde(rename = "PrevFrETSNGName")]
    pub prev_etsng_name: Option<String>,
    #[serde(rename = "FrETSNGName")]
    pub etsng_name: Option<String>,

    // --- Заявка ---
    #[serde(rename = "GU12Number")]
    pub gu12_number: Option<String>,
    #[serde(rename = "ClaimNumber")]
    pub claim_number: Option<String>,
    #[serde(rename = "ClaimDate")]
    pub claim_date: Option<String>,

    // --- Участники ---
    #[serde(rename = "Client")]
    pub client: Option<String>,
    #[serde(rename = "Sender")]
    pub sender: Option<String>,
    #[serde(rename = "Customer")]
    pub customer: Option<String>,

    // --- Тариф ---
    #[serde(rename = "Distance")]
    pub distance: i32,
    #[serde(rename = "PeriodOfDelivery")]
    pub period_of_delivery: i32,
    #[serde(rename = "Cost")]
    pub cost: f64,

    // --- Тип назначения ---
    #[serde(rename = "AssignmentType")]
    pub assignment_type: String,

    // --- Номера вагонов ---
    #[serde(rename = "CarNumbersList")]
    pub car_numbers_list: Vec<String>,

    // --- Только для Excel (не отправляется в API) ---
    #[serde(skip)]
    pub supply_kind: String,
    #[serde(skip)]
    pub period_label: String,
    /// `1` — предложение 1-х суток (АПИ); `10` — дислокация 2–10 суток.
    /// В POST АПИ попадают только записи с `supply_period == 1`.
    #[serde(skip)]
    pub supply_period: u8,
    /// Период спроса (1..4) для оптимизационных записей; 0 для "по факту".
    /// Используется только в debug-Excel, в API не отправляется.
    #[serde(skip)]
    pub demand_period: u8,
}

/// Текст поля `AssignmentType` для вагонов Assigned по `DislocationPreview.ShipmentGoalId`.
///
/// Маппинг: 1 — под погрузку; 6 — в ремонт; 8 — в промывку; 24 — в распыление;
/// иначе (включая отсутствие цели) — «По факту».
pub fn assignment_type_for_shipment_goal(goal_id: Option<i32>) -> &'static str {
    match goal_id {
        Some(1)  => "По факту под погрузку",
        Some(6)  => "По факту в ремонт",
        Some(8)  => "По факту в промывку",
        Some(24) => "По факту в распыление",
        _        => "По факту",
    }
}

/// Строит записи для вагонов `CarKind::Assigned` — они не участвуют в оптимизации.
///
/// Каждый `SupplyNode` типа `Assigned` разбивается на подзаписи по уникальным
/// станциям отправления (`station_from_code`), затем по типу назначения из
/// [`assignment_type_for_shipment_goal`] (данные `shipment_goals`: номер вагона → `ShipmentGoalId`).
///
/// Поля `StationTo` / `RailWayTo` одинаковы для всей группы (ключ группировки).
/// Тариф ищется по паре `(station_from_code, station_to_code)`.
pub fn build_assigned_output_records(
    assigned_supply: &[SupplyNode],
    tariff_nodes:    &[TariffNode],
    shipment_goals:  &HashMap<u64, Option<i32>>,
) -> Vec<OutputRecord> {
    let now_str = Local::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

    let tariff_idx: HashMap<(&str, &str), &TariffNode> = tariff_nodes
        .iter()
        .map(|t| ((t.station_from_code.as_str(), t.station_to_code.as_str()), t))
        .collect();

    let mut records: Vec<OutputRecord> = Vec::new();

    for s in assigned_supply {
        // ---------------------------------------------------------------
        // Группируем вагоны по station_from_code внутри узла.
        // Параллельные списки stations_from / railways_from / etc. строились
        // с одинаковой условной логикой, поэтому индексы соответствуют друг
        // другу внутри каждого списка (stations_from_code[i] ↔ stations_from[i]).
        // car_numbers добавляются параллельно: car_numbers[i] — i-й вагон группы;
        // stations_from_code может быть короче, если у части вагонов нет StationFrom.
        // ---------------------------------------------------------------

        // BTreeMap: from_code → (from_name, railway, railway_div, Vec<car_number>)
        let mut sub: BTreeMap<String, (String, String, Option<String>, Vec<u64>)> =
            BTreeMap::new();

        for (i, code) in s.stations_from_code.iter().enumerate() {
            let entry = sub.entry(code.clone()).or_insert_with(|| (
                s.stations_from.get(i).cloned().unwrap_or_default(),
                s.railways_from.get(i).cloned().unwrap_or_default(),
                s.railways_part_from.get(i).cloned(),
                Vec::new(),
            ));
            // Если car_numbers выровнен с stations_from_code — добавляем номер вагона.
            if let Some(&car_num) = s.car_numbers.get(i) {
                entry.3.push(car_num);
            }
        }

        // Если данных о станции отправления нет совсем — одна запись с пустыми полями.
        if sub.is_empty() {
            sub.insert(String::new(), (
                String::new(),
                String::new(),
                None,
                s.car_numbers.clone(),
            ));
        }

        // На каждую станцию отправления — отдельные записи по типу назначения (ShipmentGoalId).
        for (from_code, (from_name, rw_from, rw_div, car_nums)) in &sub {
            let tariff = tariff_idx
                .get(&(from_code.as_str(), s.station_to_code.as_str()))
                .copied();

            let mut by_assignment: BTreeMap<&'static str, Vec<u64>> = BTreeMap::new();
            for &car in car_nums {
                let gid = shipment_goals.get(&car).copied().flatten();
                let at = assignment_type_for_shipment_goal(gid);
                by_assignment.entry(at).or_default().push(car);
            }

            for (assignment_type, cars) in by_assignment {
                records.push(OutputRecord {
                    opz_date:          now_str.clone(),
                    railway_from:      rw_from.clone(),
                    railway_from_div:  rw_div.clone(),
                    station_from:      from_name.clone(),
                    station_from_code: from_code.clone(),
                    railway_to:        s.railway_to.clone(),
                    railway_to_div:    s.railway_part_to.clone(),
                    station_to:        s.station_to.clone(),
                    station_to_code:   s.station_to_code.clone(),
                    assigned_cars:     cars.len().max(1) as i32,
                    load_status:       s.status.clone(),
                    car_type:          s.car_type.clone(),
                    prev_etsng_name:   s.prev_etsng_names.first().cloned(),
                    etsng_name:        s.etsng_name.clone(),
                    gu12_number:       None,
                    claim_number:      None,
                    claim_date:        None,
                    client:            None,
                    sender:            None,
                    customer:          None,
                    distance:          tariff.map(|t| t.distance).unwrap_or(0),
                    period_of_delivery: tariff.map(|t| t.period_of_delivery).unwrap_or(0),
                    cost:              tariff.map(|t| t.cost).unwrap_or(0.0),
                    assignment_type:   assignment_type.to_string(),
                    car_numbers_list:  cars.iter().map(|n| n.to_string()).collect(),
                    supply_kind:       "Факт".to_string(),
                    period_label:      String::new(),
                    supply_period:     s.supply_period,
                    demand_period:     0,
                });
            }
        }
    }

    records
}

/// Строит список записей из результата оптимизации.
///
/// Для каждого активного узла предложения (`s_idx`) номера вагонов
/// нарезаются последовательно по дугам с ненулевым потоком: каждая
/// дуга получает ровно `qty` номеров из `SupplyNode::car_numbers`.
/// Оставшиеся вагоны (ушедшие в dummy-спрос) получают отдельную запись
/// с `assignment_type = "Затягивание грузовой операции"` и
/// `station_to == station_from` (остаются на месте).
pub fn build_output_records(
    solution: &[f64],
    arcs:     &[TaskArc],
    supply:   &[SupplyNode],
    demand:   &[DemandNode],
) -> Vec<OutputRecord> {
    let now_str = Local::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

    // --- Шаг 1: группируем активные дуги по s_idx, сохраняя порядок arc_id ---
    // Значение: Vec<(arc, qty_int)>, отсортированы по arc_id.
    let mut arcs_by_supply: HashMap<usize, Vec<(&TaskArc, i32)>> = HashMap::new();
    for (arc, qty_f) in arcs.iter().zip(solution.iter()) {
        if *qty_f < 1e-4 {
            continue;
        }
        let qty = qty_f.round() as i32;
        if qty <= 0 {
            continue;
        }
        arcs_by_supply.entry(arc.s_idx).or_default().push((arc, qty));
    }
    // Сортируем каждую группу по arc_id для детерминированного порядка нарезки.
    for group in arcs_by_supply.values_mut() {
        group.sort_unstable_by_key(|(arc, _)| arc.arc_id);
    }

    let mut records: Vec<OutputRecord> = Vec::new();

    // --- Шаг 2: для каждого узла предложения с активными дугами ---
    // Перебираем в порядке s_idx, чтобы выход был детерминирован.
    let mut sorted_s_idxs: Vec<usize> = arcs_by_supply.keys().copied().collect();
    sorted_s_idxs.sort_unstable();

    for s_idx in sorted_s_idxs {
        let s = &supply[s_idx];
        let group = &arcs_by_supply[&s_idx];

        let car_nums = &s.car_numbers;
        let mut cursor: usize = 0;

        // --- Шаг 2а: записи по назначенным дугам ---
        for &(arc, qty) in group {
            let d = &demand[arc.d_idx];

            let take = (qty as usize).min(car_nums.len().saturating_sub(cursor));
            let slice: Vec<String> = car_nums[cursor..cursor + take]
                .iter()
                .map(|n| n.to_string())
                .collect();
            cursor += take;

            let period_label = if s.supply_period == 10 {
                format!("{} (предл. 10, 2-10 сут.)", period_range_str(d.period))
            } else {
                period_range_str(d.period).to_string()
            };

            records.push(OutputRecord {
                opz_date:           now_str.clone(),
                railway_from:       s.railway_to.clone(),
                railway_from_div:   s.railway_part_to.clone(),
                station_from:       s.station_to.clone(),
                station_from_code:  s.station_to_code.clone(),
                railway_to:         d.railway_name.clone(),
                railway_to_div:     d.railway_part.clone(),
                station_to:         d.station_name.clone(),
                station_to_code:    d.station_code.clone(),
                assigned_cars:      qty,
                load_status:        s.status.clone(),
                car_type:           s.car_type.clone(),
                prev_etsng_name:    s.prev_etsng_names.first().cloned(),
                etsng_name:         s.etsng_name.clone(),
                gu12_number:        d.gu12_number.as_ref().and_then(|v| v.first().cloned()),
                claim_number:       d.request_numbers.as_ref().and_then(|v| v.first().cloned()),
                claim_date:         d.request_dates.as_ref().and_then(|v| v.first().cloned()),
                client:             d.client.as_ref().and_then(|v| v.first().cloned()),
                sender:             d.sender.clone(),
                customer:           d.recipient.as_ref().and_then(|v| v.first().cloned()),
                distance:           arc.distance,
                period_of_delivery: arc.delivery_days,
                cost:               arc.cost,
                assignment_type:    format!("Под погрузку в {period_label} сутки"),
                car_numbers_list:   slice,
                supply_kind:        car_kind_str(&s.kind).to_string(),
                period_label,
                supply_period:      s.supply_period,
                demand_period:      d.period,
            });
        }

        // --- Шаг 2б: остаток — вагоны, ушедшие в dummy (не назначены) ---
        if cursor < car_nums.len() {
            let leftover: Vec<String> = car_nums[cursor..]
                .iter()
                .map(|n| n.to_string())
                .collect();
            let leftover_count = leftover.len() as i32;
            records.push(OutputRecord {
                opz_date:           now_str.clone(),
                railway_from:       s.railway_to.clone(),
                railway_from_div:   s.railway_part_to.clone(),
                station_from:       s.station_to.clone(),
                station_from_code:  s.station_to_code.clone(),
                railway_to:         s.railway_to.clone(),
                railway_to_div:     s.railway_part_to.clone(),
                station_to:         s.station_to.clone(),
                station_to_code:    s.station_to_code.clone(),
                assigned_cars:      leftover_count,
                load_status:        s.status.clone(),
                car_type:           s.car_type.clone(),
                prev_etsng_name:    None,
                etsng_name:         None,
                gu12_number:        None,
                claim_number:       None,
                claim_date:         None,
                client:             None,
                sender:             None,
                customer:           None,
                distance:           0,
                period_of_delivery: 0,
                cost:               0.0,
                assignment_type:    "Затягивание грузовой операции".to_string(),
                car_numbers_list:   leftover,
                supply_kind:        car_kind_str(&s.kind).to_string(),
                period_label:       String::new(),
                supply_period:      s.supply_period,
                demand_period:      0,
            });
        }
    }

    // --- Шаг 3: вагоны узлов без активных дуг вовсе (весь узел — dummy) ---
    // Это узлы, у которых нет ни одной активной дуги в solution.
    for (s_idx, s) in supply.iter().enumerate() {
        if arcs_by_supply.contains_key(&s_idx) {
            continue; // уже обработан выше
        }
        // Только именные вагоны (NoNumber не имеют car_numbers).
        if s.car_numbers.is_empty() {
            continue;
        }
        records.push(OutputRecord {
            opz_date:           now_str.clone(),
            railway_from:       s.railway_to.clone(),
            railway_from_div:   s.railway_part_to.clone(),
            station_from:       s.station_to.clone(),
            station_from_code:  s.station_to_code.clone(),
            railway_to:         s.railway_to.clone(),
            railway_to_div:     s.railway_part_to.clone(),
            station_to:         s.station_to.clone(),
            station_to_code:    s.station_to_code.clone(),
            assigned_cars:      s.car_count,
            load_status:        s.status.clone(),
            car_type:           s.car_type.clone(),
            prev_etsng_name:    None,
            etsng_name:         None,
            gu12_number:        None,
            claim_number:       None,
            claim_date:         None,
            client:             None,
            sender:             None,
            customer:           None,
            distance:           0,
            period_of_delivery: 0,
            cost:               0.0,
            assignment_type:    "Затягивание грузовой операции".to_string(),
            car_numbers_list:   s.car_numbers.iter().map(|n| n.to_string()).collect(),
            supply_kind:        car_kind_str(&s.kind).to_string(),
            period_label:       String::new(),
            supply_period:      s.supply_period,
            demand_period:      0,
        });
    }

    records
}

/// Записи для тела POST в АПИ: только назначения по предложению 1-х суток (`supply_period == 1`),
/// без записей «Затягивание грузовой операции» (они только для Excel).
pub fn output_records_for_api(records: &[OutputRecord]) -> Vec<OutputRecord> {
    records
        .iter()
        .filter(|r| r.supply_period == 1 && r.assignment_type != "Затягивание грузовой операции")
        .cloned()
        .collect()
}

/// Возвращает тарифный узел с минимальной стоимостью среди всех тарифов,
/// отправление которых совпадает с `station_from_code`.
fn best_repair_tariff<'a>(
    station_from_code: &str,
    repair_tariffs: &'a [TariffNode],
) -> Option<&'a TariffNode> {
    repair_tariffs
        .iter()
        .filter(|t| t.station_from_code == station_from_code)
        .min_by(|a, b| a.cost.partial_cmp(&b.cost).unwrap_or(std::cmp::Ordering::Equal))
}

/// Строит записи для вагонов `RepairStatus::NeedsRepair` — они не участвуют в оптимизации.
///
/// Тип назначения — «В ремонт». Ремонтная станция выбирается из `repair_tariffs`
/// как станция с минимальным тарифом подсыла от текущего местонахождения вагона.
/// Если тариф не найден, станция назначения совпадает с текущей.
/// Поле `customer` заполняется из `repair_stations` по коду выбранной ремонтной станции.
pub fn build_repair_output_records(
    repair_supply:   &[SupplyNode],
    repair_tariffs:  &[TariffNode],
    repair_stations: &[RepairStation],
) -> Vec<OutputRecord> {
    let now_str = Local::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

    // Индекс: код ремонтной станции → грузополучатель (первый RecipName).
    let recip_by_code: HashMap<&str, &str> = repair_stations
        .iter()
        .filter_map(|rs| rs.recip_name.first().map(|name| (rs.station_code.as_str(), name.as_str())))
        .collect();

    repair_supply
        .iter()
        .map(|s| {
            let best = best_repair_tariff(&s.station_to_code, repair_tariffs);
            let repair_station_code = best
                .map(|t| t.station_to_code.as_str())
                .unwrap_or(s.station_to_code.as_str());
            let customer = recip_by_code.get(repair_station_code).map(|&n| n.to_string());
            OutputRecord {
                opz_date:           now_str.clone(),
                railway_from:       s.railway_to.clone(),
                railway_from_div:   s.railway_part_to.clone(),
                station_from:       s.station_to.clone(),
                station_from_code:  s.station_to_code.clone(),
                railway_to:         best.map(|t| t.railway_to.clone()).unwrap_or_else(|| s.railway_to.clone()),
                railway_to_div:     None,
                station_to:         best.map(|t| t.station_to.clone()).unwrap_or_else(|| s.station_to.clone()),
                station_to_code:    best.map(|t| t.station_to_code.clone()).unwrap_or_else(|| s.station_to_code.clone()),
                assigned_cars:      s.car_count,
                load_status:        s.status.clone(),
                car_type:           s.car_type.clone(),
                prev_etsng_name:    s.prev_etsng_names.first().cloned(),
                etsng_name:         s.etsng_name.clone(),
                gu12_number:        None,
                claim_number:       None,
                claim_date:         None,
                client:             None,
                sender:             None,
                customer,
                distance:           best.map(|t| t.distance).unwrap_or(0),
                period_of_delivery: best.map(|t| t.period_of_delivery).unwrap_or(0),
                cost:               best.map(|t| t.cost).unwrap_or(0.0),
                assignment_type:    "В ремонт".to_string(),
                car_numbers_list:   s.car_numbers.iter().map(|n| n.to_string()).collect(),
                supply_kind:        "Repair".to_string(),
                period_label:       String::new(),
                supply_period:      s.supply_period,
                demand_period:      0,
            }
        })
        .collect()
}


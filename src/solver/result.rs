use std::fs;
use std::path::PathBuf;

use chrono::Local;
use serde::{Deserialize, Serialize};

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::data::wash;
use crate::data::free_loadroads::FreeLoadRoad;
use crate::node::{CarKind, DemandNode, DemandPurpose, ReserveNode, SupplyNode, TariffNode};
use crate::data::repairs::RepairStation;
use super::lp::OptimResult;
use super::loadroads::LoadRoadAssignment;
use super::model::TaskArc;
use super::reserve::ReserveAssignment;

// ---------------------------------------------------------------------------
// Структуры отчёта
// ---------------------------------------------------------------------------

fn default_supply_period_one() -> u8 {
    1
}

/// Одна строка плана назначения: конкретный вагон (или группа) → узел спроса.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AssignmentRecord {
    /// Назначено вагонов.
    pub cars: f64,

    // --- Предложение ---
    pub supply_id:           usize,
    pub supply_kind:         String,
    /// `1` — предложение 1-х суток (АПИ); `10` — дислокация 2–10 суток.
    #[serde(default = "default_supply_period_one")]
    pub supply_period:       u8,
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
#[derive(Serialize, Deserialize, Debug, Clone)]
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
                supply_kind:         car_kind_str(&s.kind).to_string(),
                supply_period:       s.supply_period,
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
                    // prev_etsng — имя груза, который вагон везёт сейчас (SupplyNode.etsng_name).
                    // etsng в факт-записях оставляем пустым: нет целевого спроса.
                    prev_etsng_name:   s.etsng_name.clone(),
                    etsng_name:        None,
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
/// Затем нарезаются назначения в отстой (`reserve_assignments`, этап 2) —
/// записи с `assignment_type = "В отстой"`, далее размещение на путях станций
/// погрузки (`loadroad_assignments`, этап 3) — записи `assignment_type =
/// "На пути станции погрузки"`. Оставшиеся вагоны получают отдельную запись с
/// `assignment_type = "Затягивание грузовой операции"` и `station_to == station_from`
/// (остаются на месте).
#[allow(clippy::too_many_arguments)]
pub fn build_output_records(
    solution: &[f64],
    arcs:     &[TaskArc],
    supply:   &[SupplyNode],
    demand:   &[DemandNode],
    wash_codes: &HashSet<String>,
    no_cleaning_roads: &HashSet<String>,
    washed_empty_codes: &HashSet<String>,
    reserve_assignments: &[ReserveAssignment],
    reserves: &[ReserveNode],
    loadroad_assignments: &[LoadRoadAssignment],
    loadroads: &[FreeLoadRoad],
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

    // Назначения в отстой по s_idx (порядок внутри группы — порядок решателя этапа 2).
    let mut reserve_by_supply: HashMap<usize, Vec<&ReserveAssignment>> = HashMap::new();
    for ra in reserve_assignments {
        if ra.quantity > 0 {
            reserve_by_supply.entry(ra.s_idx).or_default().push(ra);
        }
    }

    // Назначения на пути станций погрузки по s_idx (этап 3, после отстоя).
    let mut loadroad_by_supply: HashMap<usize, Vec<&LoadRoadAssignment>> = HashMap::new();
    for la in loadroad_assignments {
        if la.quantity > 0 {
            loadroad_by_supply.entry(la.s_idx).or_default().push(la);
        }
    }

    let mut records: Vec<OutputRecord> = Vec::new();

    // --- Шаг 2: каждый узел предложения: дуги → отстой → остаток ---
    // Перебираем в порядке s_idx, чтобы выход был детерминирован.
    for (s_idx, s) in supply.iter().enumerate() {
        let group = arcs_by_supply.get(&s_idx).map(Vec::as_slice).unwrap_or(&[]);
        let res_group = reserve_by_supply.get(&s_idx).map(Vec::as_slice).unwrap_or(&[]);
        let load_group = loadroad_by_supply.get(&s_idx).map(Vec::as_slice).unwrap_or(&[]);
        if group.is_empty() && res_group.is_empty() && load_group.is_empty() && s.car_count <= 0 {
            continue;
        }

        let car_nums = &s.car_numbers;
        let mut cursor: usize = 0;
        let mut assigned_total: i32 = 0;

        // --- Шаг 2а: записи по назначенным дугам ---
        for &(arc, qty) in group {
            let d = &demand[arc.d_idx];

            let take = (qty as usize).min(car_nums.len().saturating_sub(cursor));
            let slice: Vec<String> = car_nums[cursor..cursor + take]
                .iter()
                .map(|n| n.to_string())
                .collect();
            cursor += take;
            assigned_total += qty;

            let period_label = if d.purpose == DemandPurpose::Wash {
                "промывка".to_string()
            } else if s.supply_period == 10 {
                format!("{} (предл. 10, 2-10 сут.)", period_range_str(d.period))
            } else {
                period_range_str(d.period).to_string()
            };

            // Тип назначения:
            // - Wash-дуга        → «в промывку»
            // - Load-дуга, грязный вагон → «под погрузку аналогичного груза»
            //   (model.rs гарантирует: такая дуга существует только при совпадении ETSNG)
            // - Load-дуга, чистый вагон → «Под погрузку в N сутки»
            let assignment_type = if d.purpose == DemandPurpose::Wash {
                "в промывку".to_string()
            } else if wash::supply_needs_wash(s, wash_codes, no_cleaning_roads, washed_empty_codes) {
                "под погрузку аналогичного груза".to_string()
            } else {
                format!("Под погрузку в {period_label} сутки")
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
                // Груз в записи назначения:
                //   prev_etsng — имя груза, который вагон везёт сейчас (SupplyNode.etsng_name).
                //   etsng     — имя груза, под который вагон назначен (DemandNode.gng_cargo —
                //               в API спроса приходит как NameGNG и является аналогом
                //               etsng_name у предложения).
                // Для Wash-спроса gng_cargo обычно пуст — поле станет None, что корректно
                // отражает смысл «назначение в промывку без конкретного груза».
                prev_etsng_name:    s.etsng_name.clone(),
                etsng_name:         d.gng_cargo.clone(),
                gu12_number:        d.gu12_number.as_ref().and_then(|v| v.first().cloned()),
                claim_number:       d.request_numbers.as_ref().and_then(|v| v.first().cloned()),
                claim_date:         d.request_dates.as_ref().and_then(|v| v.first().cloned()),
                client:             d.client.as_ref().and_then(|v| v.first().cloned()),
                sender:             d.sender.clone(),
                customer:           d.recipient.as_ref().and_then(|v| v.first().cloned()),
                distance:           arc.distance,
                period_of_delivery: arc.delivery_days,
                cost:               arc.cost,
                assignment_type,
                car_numbers_list:   slice,
                supply_kind:        car_kind_str(&s.kind).to_string(),
                period_label,
                supply_period:      s.supply_period,
                demand_period:      d.period,
            });
        }

        // --- Шаг 2б: назначения в отстой (этап 2, излишек основного решения) ---
        for ra in res_group {
            let r = &reserves[ra.r_idx];
            let take = (ra.quantity as usize).min(car_nums.len().saturating_sub(cursor));
            let slice: Vec<String> = car_nums[cursor..cursor + take]
                .iter()
                .map(|n| n.to_string())
                .collect();
            cursor += take;
            assigned_total += ra.quantity;

            records.push(OutputRecord {
                opz_date:           now_str.clone(),
                railway_from:       s.railway_to.clone(),
                railway_from_div:   s.railway_part_to.clone(),
                station_from:       s.station_to.clone(),
                station_from_code:  s.station_to_code.clone(),
                railway_to:         r.railway_short.clone(),
                railway_to_div:     r.division.clone(),
                station_to:         r.station_name.clone(),
                station_to_code:    r.station_code.clone(),
                assigned_cars:      ra.quantity,
                load_status:        s.status.clone(),
                car_type:           s.car_type.clone(),
                // В отстой: целевого груза нет, вагон едет порожним с текущим грузом.
                prev_etsng_name:    s.etsng_name.clone(),
                etsng_name:         None,
                gu12_number:        None,
                claim_number:       None,
                claim_date:         None,
                client:             None,
                sender:             None,
                customer:           r.owner.clone(),
                distance:           ra.distance,
                period_of_delivery: ra.delivery_days,
                cost:               ra.cost,
                assignment_type:    "В отстой".to_string(),
                car_numbers_list:   slice,
                supply_kind:        car_kind_str(&s.kind).to_string(),
                period_label:       "отстой".to_string(),
                supply_period:      s.supply_period,
                demand_period:      0,
            });
        }

        // --- Шаг 2в: размещение на путях станций погрузки (этап 3) ---
        for la in load_group {
            let l = &loadroads[la.l_idx];
            let take = (la.quantity as usize).min(car_nums.len().saturating_sub(cursor));
            let slice: Vec<String> = car_nums[cursor..cursor + take]
                .iter()
                .map(|n| n.to_string())
                .collect();
            cursor += take;
            assigned_total += la.quantity;

            records.push(OutputRecord {
                opz_date:           now_str.clone(),
                railway_from:       s.railway_to.clone(),
                railway_from_div:   s.railway_part_to.clone(),
                station_from:       s.station_to.clone(),
                station_from_code:  s.station_to_code.clone(),
                railway_to:         l.load_road_name.clone(),
                railway_to_div:     None,
                station_to:         l.load_station_name.clone(),
                station_to_code:    l.load_station_code.clone(),
                assigned_cars:      la.quantity,
                load_status:        s.status.clone(),
                car_type:           s.car_type.clone(),
                // Размещение на путях станции погрузки: целевого груза нет,
                // вагон едет порожним с текущим грузом.
                prev_etsng_name:    s.etsng_name.clone(),
                etsng_name:         None,
                gu12_number:        None,
                claim_number:       None,
                claim_date:         None,
                client:             None,
                sender:             None,
                customer:           None,
                distance:           la.distance,
                period_of_delivery: la.delivery_days,
                cost:               la.cost,
                assignment_type:    "На пути клиенту по договоренности или в б.о.".to_string(),
                car_numbers_list:   slice,
                supply_kind:        car_kind_str(&s.kind).to_string(),
                period_label:       "пути погрузки".to_string(),
                supply_period:      s.supply_period,
                demand_period:      0,
            });
        }

        // --- Шаг 2г: остаток — вагоны, ушедшие в dummy (не назначены) ---
        //
        // Остаток считается по количеству вагонов узла (`car_count`), а НЕ по списку
        // номеров: у NoNumber-узлов `car_numbers` пуст, но их нераспределённые вагоны
        // обязаны попасть в отчёт как «Затягивание грузовой операции» (запись без
        // номеров — так же, как их назначенные записи выше).
        // Узел без единой активной дуги и без отстоя целиком уходит сюда.
        let leftover_count = s.car_count - assigned_total;
        if leftover_count > 0 {
            let leftover: Vec<String> = car_nums[cursor.min(car_nums.len())..]
                .iter()
                .map(|n| n.to_string())
                .collect();
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
                // Затягивание грузовой операции: вагон остаётся на станции с текущим грузом.
                prev_etsng_name:    s.etsng_name.clone(),
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

    records
}

/// Баланс отчёта оптимизации: `(вагонов в записях, вагонов в предложении)`.
///
/// Суммы обязаны совпадать: каждый вагон активного предложения либо назначен
/// (запись по дуге), либо остался на месте («Затягивание грузовой операции»).
/// Расхождение означает «исчезнувшие» из отчёта вагоны — следствие ошибки
/// в [`build_output_records`] (как потеря NoNumber-остатков до фикса 2026-06).
pub fn output_balance(records: &[OutputRecord], supply: &[SupplyNode]) -> (i32, i32) {
    let cars_in_records: i32 = records.iter().map(|r| r.assigned_cars).sum();
    let cars_in_supply: i32 = supply.iter().map(|s| s.car_count).sum();
    (cars_in_records, cars_in_supply)
}

/// Записи для тела POST в АПИ: все назначения по предложению 1-х суток (`supply_period == 1`).
///
/// Вагоны дислокации (`supply_period == 10`) в АПИ не передаются — они появятся
/// через 2–10 суток и не могут быть диспетчированы немедленно.
/// «Затягивание грузовой операции» включается: вагон физически находится на станции
/// и АПИ должен отразить это состояние.
pub fn output_records_for_api(records: &[OutputRecord]) -> Vec<OutputRecord> {
    records
        .iter()
        .filter(|r| r.supply_period == 1)
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
                // В ремонт: нет целевого груза (demand отсутствует). Пишем текущий груз
                // вагона как prev_etsng; etsng остаётся None.
                prev_etsng_name:    s.etsng_name.clone(),
                etsng_name:         None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::RepairStatus;

    fn dummy_supply(count: i32, car_numbers: Vec<u64>, kind: CarKind) -> SupplyNode {
        SupplyNode {
            s_id: 1,
            kind,
            car_count: count,
            station_to: "Тестовая".to_string(),
            station_to_code: "S1".to_string(),
            railway_to: String::new(),
            railway_to_code: None,
            railway_part_to: None,
            car_type: Some("Прочие".to_string()),
            etsng: None,
            etsng_name: None,
            repair_status: RepairStatus::Ok,
            status: None,
            supply_period: 1,
            car_numbers,
            stations_from: vec![],
            stations_from_code: vec![],
            railways_from: vec![],
            railways_from_code: vec![],
            railways_part_from: vec![],
            is_mass_unloading: false,
            prev_etsngs: vec![],
            prev_etsng_names: vec![],
        }
    }

    fn dummy_demand(count: i32) -> DemandNode {
        DemandNode {
            d_id: 1,
            purpose: DemandPurpose::Load,
            period: 1,
            station_name: "Погрузка".to_string(),
            station_code: "D1".to_string(),
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
            shipping_type: None,
            car_type: Some("Прочие".to_string()),
            car_count: count,
            cars_on_station: 0,
        }
    }

    fn dummy_arc() -> TaskArc {
        TaskArc {
            arc_id: 0,
            s_idx: 0,
            d_idx: 0,
            supply_station_code: "S1".to_string(),
            demand_station_code: "D1".to_string(),
            cost: 100.0,
            distance: 1,
            delivery_days: 1,
            period_ok: true,
            car_type_ok: true,
            pair_min_batch: 0,
        }
    }

    fn build(solution: &[f64], arcs: &[TaskArc], supply: &[SupplyNode], demand: &[DemandNode]) -> Vec<OutputRecord> {
        build_output_records(
            solution, arcs, supply, demand,
            &HashSet::new(), &HashSet::new(), &HashSet::new(), &[], &[], &[], &[],
        )
    }

    fn dummy_reserve(capacity: i32) -> ReserveNode {
        ReserveNode {
            r_id: 1,
            station_name: "Отстойная".to_string(),
            station_code: "R1".to_string(),
            railway_short: "МСК".to_string(),
            railway_code: None,
            division: None,
            owner: Some("ООО Отстой".to_string()),
            owner_okpo: None,
            agreement_number: None,
            capacity,
        }
    }

    /// NoNumber-узел с частичным назначением: остаток обязан получить запись
    /// «Затягивание грузовой операции» по количеству (номеров нет).
    #[test]
    fn no_number_leftover_gets_zatyagivanie() {
        let supply = vec![dummy_supply(5, vec![], CarKind::NoNumber)];
        let demand = vec![dummy_demand(3)];
        let arcs = vec![dummy_arc()];
        let records = build(&[3.0], &arcs, &supply, &demand);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].assigned_cars, 3);
        let leftover = &records[1];
        assert_eq!(leftover.assignment_type, "Затягивание грузовой операции");
        assert_eq!(leftover.assigned_cars, 2);
        assert!(leftover.car_numbers_list.is_empty());

        let (recs, sup) = output_balance(&records, &supply);
        assert_eq!(recs, sup);
    }

    /// NoNumber-узел без единой активной дуги: весь узел уходит в «Затягивание».
    #[test]
    fn no_number_node_without_arcs_gets_record() {
        let supply = vec![dummy_supply(4, vec![], CarKind::NoNumber)];
        let demand = vec![dummy_demand(3)];
        let arcs = vec![dummy_arc()];
        let records = build(&[0.0], &arcs, &supply, &demand);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].assignment_type, "Затягивание грузовой операции");
        assert_eq!(records[0].assigned_cars, 4);

        let (recs, sup) = output_balance(&records, &supply);
        assert_eq!(recs, sup);
    }

    /// Номерной узел: остаток получает «Затягивание» с конкретными номерами вагонов.
    #[test]
    fn numbered_leftover_keeps_numbers() {
        let supply = vec![dummy_supply(3, vec![101, 102, 103], CarKind::Free)];
        let demand = vec![dummy_demand(2)];
        let arcs = vec![dummy_arc()];
        let records = build(&[2.0], &arcs, &supply, &demand);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].assigned_cars, 2);
        assert_eq!(records[0].car_numbers_list, vec!["101", "102"]);
        let leftover = &records[1];
        assert_eq!(leftover.assignment_type, "Затягивание грузовой операции");
        assert_eq!(leftover.assigned_cars, 1);
        assert_eq!(leftover.car_numbers_list, vec!["103"]);

        let (recs, sup) = output_balance(&records, &supply);
        assert_eq!(recs, sup);
    }

    /// Полностью назначенный узел не получает записи «Затягивание».
    #[test]
    fn fully_assigned_node_has_no_leftover_record() {
        let supply = vec![dummy_supply(2, vec![201, 202], CarKind::Free)];
        let demand = vec![dummy_demand(2)];
        let arcs = vec![dummy_arc()];
        let records = build(&[2.0], &arcs, &supply, &demand);

        assert_eq!(records.len(), 1);
        let (recs, sup) = output_balance(&records, &supply);
        assert_eq!(recs, sup);
    }

    /// Излишек после дуги частично уходит в отстой: запись «В отстой» получает
    /// следующие номера по курсору, остаток — «Затягивание», баланс сходится.
    #[test]
    fn reserve_assignment_consumes_cursor_and_balances() {
        let supply = vec![dummy_supply(5, vec![101, 102, 103, 104, 105], CarKind::Free)];
        let demand = vec![dummy_demand(2)];
        let arcs = vec![dummy_arc()];
        let reserves = vec![dummy_reserve(10)];
        let ra = vec![ReserveAssignment {
            s_idx: 0, r_idx: 0, quantity: 2,
            cost: 12_000.0, distance: 250, delivery_days: 3,
        }];
        let records = build_output_records(
            &[2.0], &arcs, &supply, &demand,
            &HashSet::new(), &HashSet::new(), &HashSet::new(), &ra, &reserves, &[], &[],
        );

        assert_eq!(records.len(), 3);
        assert_eq!(records[0].car_numbers_list, vec!["101", "102"]);

        let reserve_rec = &records[1];
        assert_eq!(reserve_rec.assignment_type, "В отстой");
        assert_eq!(reserve_rec.assigned_cars, 2);
        assert_eq!(reserve_rec.car_numbers_list, vec!["103", "104"]);
        assert_eq!(reserve_rec.station_to, "Отстойная");
        assert_eq!(reserve_rec.station_to_code, "R1");
        assert_eq!(reserve_rec.customer.as_deref(), Some("ООО Отстой"));
        assert_eq!(reserve_rec.cost, 12_000.0);

        let leftover = &records[2];
        assert_eq!(leftover.assignment_type, "Затягивание грузовой операции");
        assert_eq!(leftover.assigned_cars, 1);
        assert_eq!(leftover.car_numbers_list, vec!["105"]);

        let (recs, sup) = output_balance(&records, &supply);
        assert_eq!(recs, sup);
    }

    fn dummy_loadroad(code: &str, free: i64) -> FreeLoadRoad {
        FreeLoadRoad {
            load_road_name: "ПРВ".to_string(),
            load_station_name: "Погрузочная".to_string(),
            load_station_code: code.to_string(),
            rail_road_capacity: free + 10,
            cars_on_rail_roads: 10,
            free_rail_road_capacity: free,
        }
    }

    /// После дуги и отстоя остаток уходит на пути станции погрузки (этап 3):
    /// запись «На пути станции погрузки» забирает номера по курсору, баланс сходится.
    #[test]
    fn loadroad_assignment_consumes_cursor_and_balances() {
        let supply = vec![dummy_supply(7, vec![101, 102, 103, 104, 105, 106, 107], CarKind::Free)];
        let demand = vec![dummy_demand(2)];
        let arcs = vec![dummy_arc()];
        let reserves = vec![dummy_reserve(10)];
        let ra = vec![ReserveAssignment {
            s_idx: 0, r_idx: 0, quantity: 2,
            cost: 12_000.0, distance: 250, delivery_days: 3,
        }];
        let roads = vec![dummy_loadroad("L1", 100)];
        let la = vec![LoadRoadAssignment {
            s_idx: 0, l_idx: 0, quantity: 3,
            cost: 9_000.0, distance: 120, delivery_days: 2,
        }];
        let records = build_output_records(
            &[2.0], &arcs, &supply, &demand,
            &HashSet::new(), &HashSet::new(), &HashSet::new(), &ra, &reserves, &la, &roads,
        );

        // дуга(2) + отстой(2) + пути(3) = 7, остатка нет → 3 записи.
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].car_numbers_list, vec!["101", "102"]);
        assert_eq!(records[1].assignment_type, "В отстой");
        assert_eq!(records[1].car_numbers_list, vec!["103", "104"]);

        let road_rec = &records[2];
        assert_eq!(road_rec.assignment_type, "На пути клиенту по договоренности или в б.о.");
        assert_eq!(road_rec.assigned_cars, 3);
        assert_eq!(road_rec.car_numbers_list, vec!["105", "106", "107"]);
        assert_eq!(road_rec.station_to, "Погрузочная");
        assert_eq!(road_rec.station_to_code, "L1");
        assert_eq!(road_rec.railway_to, "ПРВ");
        assert_eq!(road_rec.cost, 9_000.0);

        let (recs, sup) = output_balance(&records, &supply);
        assert_eq!(recs, sup);
    }

    /// Узел без активных дуг, целиком ушедший в отстой, не получает «Затягивание».
    #[test]
    fn node_fully_in_reserve_without_arcs() {
        let supply = vec![dummy_supply(3, vec![301, 302, 303], CarKind::Free)];
        let demand = vec![dummy_demand(2)];
        let arcs = vec![dummy_arc()];
        let reserves = vec![dummy_reserve(5)];
        let ra = vec![ReserveAssignment {
            s_idx: 0, r_idx: 0, quantity: 3,
            cost: 8_000.0, distance: 100, delivery_days: 2,
        }];
        let records = build_output_records(
            &[0.0], &arcs, &supply, &demand,
            &HashSet::new(), &HashSet::new(), &HashSet::new(), &ra, &reserves, &[], &[],
        );

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].assignment_type, "В отстой");
        assert_eq!(records[0].assigned_cars, 3);
        assert_eq!(records[0].car_numbers_list, vec!["301", "302", "303"]);

        let (recs, sup) = output_balance(&records, &supply);
        assert_eq!(recs, sup);
    }
}


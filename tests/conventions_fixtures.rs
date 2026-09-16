//! Шаг 6 правила 5: конвенции РЖД без живого Redis.
//!
//! Фикстуры `tests/fixtures/conventions/*.json` — массив значений HASH `telegrams_db`
//! в том виде, как их пишет `railconventions`. Каждый сценарий проходит **весь** путь:
//! JSON → `parse_hash` (даты, класс, дороги) → `ConventionIndex` → `build_task_arcs` /
//! отстой / ремонт. Юнит-тесты индекса собирают `ParsedConvention` руками и не ловят
//! расхождений разбора с индексом (так была потеряна дорожная пара у 4702).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use railoptim::data::conventions::{parse_hash, ConventionsLoad, RailwayCatalog, RAILWAY_MAP_PATH};
use railoptim::data::repairs::RepairStation;
use railoptim::data::{BusinessRules, ConventionIndex, StationBacklogIndex};
use railoptim::node::{
    CarKind, DemandNode, DemandPurpose, RepairStatus, ReserveNode, SupplyNode, TariffNode,
};
use railoptim::solver::model::{ArcStats, TaskArc};
use railoptim::solver::{build_repair_output_records, build_task_arcs, solve_reserve_assignment};

// --- Загрузка фикстур ----------------------------------------------------------

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 15).unwrap()
}

fn catalog() -> RailwayCatalog {
    RailwayCatalog::load_csv(&root().join(RAILWAY_MAP_PATH)).expect("справочник дорог")
}

/// Массив телеграмм из файла → HASH `{rzd_number: raw json}` (как `HGETALL`).
fn hash_from_file(path: &Path) -> HashMap<String, String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let items: Vec<serde_json::Value> = serde_json::from_str(&text).expect("массив телеграмм");
    items
        .into_iter()
        .map(|v| {
            let key = v["rzd_number"].as_str().expect("rzd_number").to_string();
            (key, v.to_string())
        })
        .collect()
}

fn hash_from_fixture(name: &str) -> HashMap<String, String> {
    hash_from_file(&root().join("tests/fixtures/conventions").join(name))
}

fn load_fixture(name: &str) -> ConventionsLoad {
    parse_hash(&hash_from_fixture(name), today(), &catalog())
}

fn index_of(load: ConventionsLoad) -> ConventionIndex {
    ConventionIndex::build_at(load.active, today())
}

// --- Узлы -------------------------------------------------------------------

fn supply(code: &str, rw: &str, cars: i32) -> SupplyNode {
    SupplyNode {
        s_id: 0,
        kind: CarKind::Free,
        car_count: cars,
        station_to: format!("Ст-{code}"),
        station_to_code: code.into(),
        railway_to: rw.into(),
        railway_to_code: None,
        railway_part_to: None,
        car_type: None,
        etsng: None,
        etsng_name: None,
        repair_status: RepairStatus::Ok,
        status: None,
        supply_period: 1,
        car_numbers: vec![],
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

fn demand(code: &str, name: &str, rw: &str, cars: i32) -> DemandNode {
    DemandNode {
        d_id: 0,
        purpose: DemandPurpose::Load,
        period: 1,
        station_name: name.into(),
        station_code: code.into(),
        railway_name: rw.into(),
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
        car_type: None,
        car_count: cars,
        cars_on_station: 0,
    }
}

fn cargo_to(mut d: DemandNode, code: &str, name: &str, rw: &str) -> DemandNode {
    d.station_to_code = Some(code.into());
    d.station_to_name = Some(name.into());
    d.railway_to_name = Some(rw.into());
    d
}

fn sender(mut d: DemandNode, okpo: Option<&str>, name: Option<&str>) -> DemandNode {
    d.sender_okpo = okpo.map(str::to_string);
    d.sender = name.map(str::to_string);
    d
}

fn tariff(from: &str, to: &str, days: i32) -> TariffNode {
    TariffNode {
        station_from: format!("Ст-{from}"),
        station_from_code: from.into(),
        railway_from: String::new(),
        railway_from_code: 0,
        station_to: format!("Ст-{to}"),
        station_to_code: to.into(),
        railway_to: String::new(),
        railway_to_code: 0,
        distance: 500,
        period_of_delivery: days,
        cost: 10_000.0,
        actual_date: Default::default(),
    }
}

/// `build_task_arcs` только с правилом 5 (остальные фильтры выключены).
fn arcs(
    supply: &[SupplyNode],
    demand: &[DemandNode],
    tariffs: &[TariffNode],
    idx: &ConventionIndex,
) -> (Vec<TaskArc>, ArcStats) {
    build_task_arcs(
        supply,
        demand,
        tariffs,
        &HashSet::new(),
        &HashSet::new(),
        &HashMap::new(),
        &BusinessRules::default(),
        &StationBacklogIndex::disabled(),
        idx,
    )
}

fn arc_demands(arcs: &[TaskArc]) -> Vec<usize> {
    let mut v: Vec<usize> = arcs.iter().map(|a| a.d_idx).collect();
    v.sort_unstable();
    v
}

// --- Сценарии плана (шаг 6) -----------------------------------------------------

/// Empty + один ЕСР + два отправителя (ОКПО 50 и 31): закрыт только совпавший ОКПО/имя.
#[test]
fn empty_esr_two_senders_only_matching_is_closed() {
    let load = load_fixture("empty_esr_two_senders.json");
    assert_eq!(load.stats.active, 1);
    assert!(!load.active[0].all_parties);
    assert_eq!(load.active[0].recipient_okpo, vec!["50"], "ведущие нули ОКПО сняты");
    let idx = index_of(load);

    let s = supply("100001", "ПРВ", 10);
    let by_okpo = sender(demand("987303", "Руденск", "ПРВ", 3), Some("00000050"), Some("ООО Другое имя"));
    let by_name = sender(demand("987303", "Руденск", "ПРВ", 3), Some("31"), Some("ООО \"Пятьдесят\""));
    let other = sender(demand("987303", "Руденск", "ПРВ", 3), Some("31"), Some("ООО Тридцать один"));
    let other_station = sender(demand("100002", "Соседняя", "ПРВ", 3), Some("50"), Some("ООО Пятьдесят"));
    let tariffs = vec![tariff("100001", "987303", 2), tariff("100001", "100002", 2)];

    let (a, st) = arcs(&[s], &[by_okpo, by_name, other, other_station], &tariffs, &idx);
    assert_eq!(arc_demands(&a), vec![2, 3], "закрыты только узлы с ОКПО 50 или именем «Пятьдесят»");
    assert_eq!(st.convention_ban, 2);
    assert_eq!(st.convention_by_number.get("9001"), Some(&2));
}

/// Grain + «Новороссийск эксп.»: закрыты узлы с `station_to` этой станции, погрузка на ней — открыта.
#[test]
fn grain_novorossiysk_closes_cargo_destination_not_loading_station() {
    let load = load_fixture("grain_novorossiysk.json");
    assert_eq!(load.stats.active_grain, 1);
    assert!(load.active[0].all_parties);
    assert_eq!(load.active[0].dest_esr, vec!["514003"]);
    let idx = index_of(load);

    let s = supply("100001", "СКВ", 10);
    let to_novoros_by_esr = cargo_to(demand("300001", "Элеватор А", "СКВ", 3), "514003", "Новороссийск", "СКВ");
    let to_novoros_by_name = cargo_to(demand("300002", "Элеватор Б", "ПРВ", 3), "", "Новороссийск (эксп.)", "СКВ");
    let load_at_novoros = cargo_to(demand("514003", "Новороссийск (эксп.)", "СКВ", 3), "200002", "Другая", "ОКТ");
    let elsewhere = cargo_to(demand("300003", "Элеватор В", "СКВ", 3), "200002", "Другая", "ОКТ");
    let tariffs = vec![
        tariff("100001", "300001", 2),
        tariff("100001", "300002", 2),
        tariff("100001", "514003", 2),
        tariff("100001", "300003", 2),
    ];

    let (a, st) = arcs(&[s], &[to_novoros_by_esr, to_novoros_by_name, load_at_novoros, elsewhere], &tariffs, &idx);
    assert_eq!(arc_demands(&a), vec![2, 3]);
    assert_eq!(st.convention_ban, 2);
}

/// Клон 4702 (All, все станции КРС/ЗСБ/ЮУР/СВР → ОКТ/СЕВ/МСК/ГОР, получатель All):
/// ЗСБ→ОКТ закрыт, ЗСБ→СКВ открыт, МСК→ОКТ открыт (погрузка не с dep-дорог).
#[test]
fn clone_4702_closes_only_dep_to_dest_road_pair() {
    let load = load_fixture("all_roads_4702_open_ended.json");
    assert_eq!(load.stats.active_all, 1);
    let r = &load.active[0];
    assert!(r.dest_names.is_empty(), "дороги не должны попадать в имена станций: {:?}", r.dest_names);
    let idx = index_of(load);
    assert_eq!(idx.stats.road_pair_rules, 1, "{}", idx.summary_line());
    assert_eq!(idx.stats.empty_load_esr_keys, 0);
    assert_eq!(idx.stats.not_indexed, 0);

    let s = supply("100001", "ЗСБ", 10);
    let zsb_okt = cargo_to(demand("300001", "А", "ЗСБ", 3), "200001", "Б", "ОКТ");
    let zsb_skv = cargo_to(demand("300002", "А", "ЗСБ", 3), "200002", "Б", "СКВ");
    let msk_okt = cargo_to(demand("300003", "А", "МСК", 3), "200003", "Б", "ОКТ");
    let svr_gor = cargo_to(demand("300004", "А", "СВР", 3), "200004", "Б", "ГОР");
    let tariffs = vec![
        tariff("100001", "300001", 2),
        tariff("100001", "300002", 2),
        tariff("100001", "300003", 2),
        tariff("100001", "300004", 2),
    ];

    let (a, st) = arcs(&[s], &[zsb_okt, zsb_skv, msk_okt, svr_gor], &tariffs, &idx);
    assert_eq!(arc_demands(&a), vec![1, 2]);
    assert_eq!(st.convention_ban, 2);
    assert_eq!(st.convention_by_number.get("4702"), Some(&2));
}

/// Истёкший `date_end` (реальная 4702 из репозитория) — запрет не действует.
#[test]
fn expired_4702_example_is_not_active() {
    // В репозитории пример — один объект (одно поле HASH), не массив.
    let path = root().join("data/conventions/telegram_4702.example.json");
    let text = std::fs::read_to_string(&path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let raw: HashMap<String, String> = [("4702".to_string(), v.to_string())].into();
    let load = parse_hash(&raw, today(), &catalog());
    assert_eq!(load.stats.hash_fields, 1);
    assert_eq!(load.stats.expired, 1);
    assert_eq!(load.stats.active, 0);
    let idx = index_of(load);
    assert!(idx.is_empty());

    let s = supply("100001", "ЗСБ", 10);
    let zsb_okt = cargo_to(demand("300001", "А", "ЗСБ", 3), "200001", "Б", "ОКТ");
    let (a, st) = arcs(&[s], &[zsb_okt], &[tariff("100001", "300001", 2)], &idx);
    assert_eq!(a.len(), 1);
    assert_eq!(st.convention_ban, 0);
}

/// `date_end` = `3000-01-01` («до отмены») — действует.
#[test]
fn open_ended_date_is_active() {
    let load = load_fixture("uzbek_empty_all_stations.json");
    assert_eq!(load.stats.active, 1);
    assert_eq!(load.active[0].date_end, "3000-01-01");
    assert_eq!(load.stats.expired, 0);
    assert_eq!(load.stats.not_yet, 0);
}

/// КЗХ «все станции» — игнор (даже при стыке в телеграмме).
#[test]
fn kzh_all_stations_is_ignored() {
    let load = load_fixture("kzh_all_stations.json");
    assert_eq!(load.stats.skipped_kzh, 1);
    assert_eq!(load.stats.active, 0);
    assert_eq!(load.stats.unresolved, 0);
    let idx = index_of(load);
    assert!(idx.is_empty());

    let s = supply("100001", "ПРВ", 10);
    let to_kzh = cargo_to(demand("300001", "А", "ПРВ", 3), "700001", "Б", "КЗХ");
    let on_kzh = cargo_to(demand("700002", "А", "КЗХ", 3), "300001", "Б", "ПРВ");
    let (a, _) = arcs(&[s], &[to_kzh, on_kzh], &[tariff("100001", "300001", 2), tariff("100001", "700002", 2)], &idx);
    assert_eq!(a.len(), 2);
}

/// Битый JSON в одном поле HASH — загрузчик жив, остальные телеграммы разобраны.
#[test]
fn broken_json_field_does_not_kill_loader() {
    let mut raw = hash_from_fixture("grain_novorossiysk.json");
    raw.insert("broken-1".into(), "{not json at all".into());
    raw.insert("broken-2".into(), r#"{"rzd_number": "x", "date_beg": 1}"#.into());
    raw.insert("broken-3".into(), String::new());
    let load = parse_hash(&raw, today(), &catalog());
    assert_eq!(load.stats.hash_fields, 4);
    assert_eq!(load.stats.bad_json, 3);
    assert_eq!(load.stats.parsed_ok, 1);
    assert_eq!(load.stats.active, 1);
    assert_eq!(load.active[0].rzd_number, "9002");
}

/// `Others` и `LLM_ERROR` — не попадают в индекс.
#[test]
fn others_and_llm_error_never_reach_index() {
    let load = load_fixture("others_covered_wagons.json");
    assert_eq!(load.stats.parsed_ok, 2);
    assert_eq!(load.stats.skipped_class, 2);
    assert_eq!(load.stats.active, 0);
    let idx = index_of(load);
    assert!(idx.is_empty());

    let s = supply("100001", "ПРВ", 10);
    let on_skv = cargo_to(demand("300001", "А", "СКВ", 3), "200001", "Б", "СКВ");
    let (a, _) = arcs(&[s], &[on_skv], &[tariff("100001", "300001", 2)], &idx);
    assert_eq!(a.len(), 1);
}

// --- Сверх плана: найдено на ревью -----------------------------------------------

/// «Все станции Узбекской ЖД» (как №27638 в боевом снимке): УЗБ распознаётся,
/// подсыл на станции погрузки УЗБ закрыт, на СКВ — открыт.
#[test]
fn uzbek_all_stations_empty_closes_loading_stations_on_uzb() {
    let load = load_fixture("uzbek_empty_all_stations.json");
    assert_eq!(load.stats.active_empty, 1);
    assert_eq!(load.stats.unresolved, 0);
    assert_eq!(load.active[0].dest_railways, vec!["УЗБ"]);
    let idx = index_of(load);
    assert_eq!(idx.stats.not_indexed, 0, "{}", idx.summary_line());

    let s = supply("100001", "ПРВ", 10);
    let on_uzb = demand("720001", "Ташкент-Товарный", "УЗБ", 3);
    let on_skv = demand("300001", "А", "СКВ", 3);
    let (a, st) = arcs(&[s], &[on_uzb, on_skv], &[tariff("100001", "720001", 6), tariff("100001", "300001", 2)], &idx);
    assert_eq!(arc_demands(&a), vec![1]);
    assert_eq!(st.convention_by_number.get("27638"), Some(&1));
}

/// Нераспознанная дорога — телеграмма не в индексе, но видна в `unresolved` для лога.
#[test]
fn unresolved_road_is_reported_and_not_applied() {
    let load = load_fixture("unresolved_road.json");
    assert_eq!(load.stats.active, 0);
    assert_eq!(load.stats.unresolved, 1);
    assert_eq!(load.unresolved[0].rzd_number, "9007");
    assert_eq!(load.unresolved[0].unknown_road_fragments, vec!["марсианской"]);
    let idx = index_of(load);
    assert!(idx.is_empty());
}

/// Телеграммы промывки / ремонта / отстоя (класс Others → Empty): закрывают только свой
/// вид назначения; погрузка на той же станции остаётся открытой.
#[test]
fn service_station_telegrams_close_only_their_scope() {
    let load = load_fixture("service_stations.json");
    assert_eq!(load.stats.active, 3);
    assert_eq!(load.stats.active_service, 3);
    assert_eq!(load.stats.skipped_class, 0, "Others у промывки/ремонта/отстоя — это Empty");
    let idx = index_of(load);
    assert_eq!(idx.stats.wash_rules, 1);
    assert_eq!(idx.stats.repair_rules, 1);
    assert_eq!(idx.stats.reserve_rules, 1);

    // Промывка: узел Wash на 555001 закрыт, Load на 555001 — открыт.
    let mut dirty = supply("100001", "СКВ", 10);
    dirty.prev_etsngs = vec!["421034".into()];
    let mut wash = demand("555001", "Промывочная", "СКВ", 10);
    wash.purpose = DemandPurpose::Wash;
    let mut load_same_station = demand("555001", "Промывочная", "СКВ", 3);
    load_same_station.etsng = Some("421034".into());
    let wash_codes: HashSet<String> = ["421034".to_string()].into_iter().collect();
    let mut wash_tariffs = HashMap::new();
    wash_tariffs.insert(("100001".to_string(), "555001".to_string()), tariff("100001", "555001", 2));
    let (a, st) = build_task_arcs(
        &[dirty],
        &[wash, load_same_station],
        &[tariff("100001", "555001", 2)],
        &wash_codes,
        &HashSet::new(),
        &wash_tariffs,
        &BusinessRules::default(),
        &StationBacklogIndex::disabled(),
        &idx,
    );
    assert_eq!(arc_demands(&a), vec![1], "промывка закрыта, погрузка открыта");
    assert_eq!(st.convention_by_number.get("W-555001"), Some(&1));

    // Ремонт: 666001 закрыта → берётся следующая по тарифу 666002.
    let mut broken = supply("100001", "МСК", 2);
    broken.repair_status = RepairStatus::NeedsRepair;
    let repair_tariffs = vec![
        {
            let mut t = tariff("100001", "666001", 2);
            t.cost = 5_000.0;
            t
        },
        {
            let mut t = tariff("100001", "666002", 2);
            t.cost = 9_000.0;
            t
        },
    ];
    let stations = vec![
        RepairStation {
            railway: "МСК".into(),
            station_name: "Ремонтная".into(),
            station_code: "666001".into(),
            recip_name: vec!["АО ВРК-1".into()],
            recip_okpo: vec![],
        },
        RepairStation {
            railway: "МСК".into(),
            station_name: "Ремонтная-2".into(),
            station_code: "666002".into(),
            recip_name: vec!["АО ВРК-2".into()],
            recip_okpo: vec![],
        },
    ];
    let recs = build_repair_output_records(&[broken], &repair_tariffs, &stations, &idx);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].station_to_code, "666002");
    assert_eq!(recs[0].customer.as_deref(), Some("АО ВРК-2"));

    // Отстой: 777001 закрыта → излишек уходит на 777002 при более дорогом тарифе.
    let excess_supply = supply("100001", "ПРВ", 4);
    let reserves = vec![
        ReserveNode {
            r_id: 1,
            station_name: "Отстойная".into(),
            station_code: "777001".into(),
            railway_short: "ПРВ".into(),
            railway_code: None,
            division: None,
            owner: Some("ООО Отстой".into()),
            owner_okpo: None,
            agreement_number: None,
            capacity: 10,
        },
        ReserveNode {
            r_id: 2,
            station_name: "Отстойная-2".into(),
            station_code: "777002".into(),
            railway_short: "ПРВ".into(),
            railway_code: None,
            division: None,
            owner: Some("ООО Отстой-2".into()),
            owner_okpo: None,
            agreement_number: None,
            capacity: 10,
        },
    ];
    let mut reserve_tariffs = HashMap::new();
    reserve_tariffs.insert(("100001".to_string(), "777001".to_string()), {
        let mut t = tariff("100001", "777001", 1);
        t.cost = 1_000.0;
        t
    });
    reserve_tariffs.insert(("100001".to_string(), "777002".to_string()), {
        let mut t = tariff("100001", "777002", 1);
        t.cost = 50_000.0;
        t
    });
    let assigned = solve_reserve_assignment(&[4], &[excess_supply], &reserves, &reserve_tariffs, &idx);
    assert_eq!(assigned.len(), 1);
    assert_eq!(assigned[0].r_idx, 1);
    assert_eq!(assigned[0].quantity, 4);
}

/// Перечень ЕСР + класс All (как №19412): закрыт и подсыл порожняка на эти станции,
/// и погрузка назначением на них; узел в стороне — открыт. Имена станций не считаются
/// «нераспознанными дорогами».
#[test]
fn all_class_station_list_closes_empty_and_cargo_sides() {
    let load = load_fixture("all_station_list_19412_like.json");
    assert_eq!(load.stats.active_all, 1);
    let r = &load.active[0];
    assert_eq!(r.dest_esr.len(), 5);
    assert!(r.unknown_road_fragments.is_empty(), "{:?}", r.unknown_road_fragments);
    assert!(!r.dest_all_stations);
    let idx = index_of(load);
    assert_eq!(idx.stats.empty_load_esr_keys, 5);
    assert_eq!(idx.stats.grain_cargo_esr_keys, 5);

    let s = supply("100001", "МСК", 10);
    let load_at_rylsk = cargo_to(demand("207603", "Рыльск", "МСК", 3), "300001", "Б", "СКВ");
    let cargo_to_sudzha = cargo_to(demand("300002", "А", "СКВ", 3), "206507", "Суджа", "МСК");
    let unrelated = cargo_to(demand("300003", "А", "СКВ", 3), "300004", "Б", "СКВ");
    let tariffs = vec![
        tariff("100001", "207603", 1),
        tariff("100001", "300002", 2),
        tariff("100001", "300003", 2),
    ];
    let (a, st) = arcs(&[s], &[load_at_rylsk, cargo_to_sudzha, unrelated], &tariffs, &idx);
    assert_eq!(arc_demands(&a), vec![2]);
    assert_eq!(st.convention_ban, 2);
}

/// Все фикстуры разбираются как `TelegramData` (форма HASH не разошлась с `railconventions`).
#[test]
fn every_fixture_has_redis_shape() {
    let dir = root().join("tests/fixtures/conventions");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        for (key, raw) in hash_from_file(&path) {
            let t: railoptim::data::TelegramData =
                serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{}: №{key}: {e}", path.display()));
            assert_eq!(t.rzd_number, key);
            n += 1;
        }
    }
    assert!(n >= 10, "фикстур: {n}");
}

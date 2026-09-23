use std::collections::{HashMap, HashSet};

use anyhow::Result;
use railoptim::config::Config;
use railoptim::data::{self, ApiClient, StationRef};
use railoptim::node::{CarKind, DemandNode, DemandPurpose, RepairStatus, ReserveNode, TariffNode};
use railoptim::{debug,solver};


#[tokio::main]
async fn main() -> Result<()> {
    // -----------------------------------------------------------------------
    // 1. Конфигурация и API-клиент
    // -----------------------------------------------------------------------
    // cfg живёт только внутри блока: токен копируется в default headers клиента,
    // затем SecretString затирается при drop. Authorization в client не зависит от env.
    let client = {
        let cfg = Config::from_env()?;
        ApiClient::new(&cfg.api_base_url, &cfg.api_token)?
    };
    // Убираем токен из environ процесса (читается через /proc); на запросы не влияет.
    // SAFETY: вызывается один раз при старте; после этого код не читает API_TOKEN из env,
    // авторизация только через default headers в `client`.
    // это unsafe! контролировать запуск других сервисов с тем же токеном!
    unsafe { std::env::remove_var("API_TOKEN") };

    // Справочник станций ЕСР + координаты (опционально; не блокирует оптимизацию).
    let _stations_geo = data::StationGeoCatalog::load_from_env();

    // -----------------------------------------------------------------------
    // 2. Получение данных спроса и предложения
    // -----------------------------------------------------------------------
    let mut demand_nodes = client.fetch_demand_nodes().await?;
    let demand_total_cars: i32 = demand_nodes.iter()
                .map(|d| d.car_count)
                .sum();
    println!("Получено узлов спроса (погрузка): {} или {} вагонов", demand_nodes.len(), demand_total_cars);

    // Бизнес-правила логистов (data/business_rules.json): потолок дальности подсыла
    // под погрузку, в отстой и на пути клиента, инотерритории, дефицитные дороги (дуги погрузки),
    // проверка ГУ-12 (спрос),
    // грязный вагон под аналогичный груз (правило 6: стоимость промывочного маршрута,
    // потолок и поощрение), вывод в ремонт (правило 7: 15 сут. / 45 сут. на инотерритории),
    // иномойка (правило 8: не грязные; капризные дороги при профиците),
    // поправка периода 1 по расстоянию (только модель, не отчёт; в режиме --day1 выключена —
    // без периода 10 ей нечего балансировать).
    // Не загрузились => правил 1–2 нет, ограничения на дуги не применяются, потолок отстоя/путей
    // выкл., правило 6 — с прежними константами (10 000 + 40 000) без поощрения, правило 7 —
    // 15/45 сут. без списка инотерриторий (все дороги как российские), правило 8 выключено.
    let include_period10 = include_period10_from_env();
    let business_rules = match data::BusinessRules::load("data/business_rules.json") {
        Ok(mut r) => {
            let p1_adjust_off_by_day1 = !include_period10 && r.p1_distance_adjust_enabled();
            if p1_adjust_off_by_day1 {
                r.disable_p1_distance_adjust();
            }
            println!(
                "Бизнес-правила (business_rules.json): потолок подсыла {}; потолок отстоя/путей {}; инотерриторий {} (исключений {}); дефицитных дорог {} (вывоз ≤ {} км, надбавка {:.0} руб.); проверка ГУ-12 {}; загруженность станций (правило 4) {}; конвенции РЖД (правило 5) {}; грязный под свой груз (правило 6): промывочный маршрут +{:.0}+{:.0} руб., потолок {}, поощрение {}; ремонт (правило 7): {} сут. / инотерритория {} сут.; иномойка (правило 8): {} дорог образования, капризных {} (надбавка {:.0} руб. при предложении > {:.2} × спроса); поправка периода 1 по расстоянию: {}",
                r.max_empty_run_distance_km
                    .map(|km| format!("{km} км"))
                    .unwrap_or_else(|| "выкл.".to_string()),
                match r.max_reserve_empty_run_distance_km {
                    None => "выкл.".to_string(),
                    Some(base) => {
                        if r.reserve_far_east_railways.is_empty() {
                            format!("{base} км")
                        } else {
                            let mut roads: Vec<_> =
                                r.reserve_far_east_railways.iter().cloned().collect();
                            roads.sort();
                            let far = r.max_reserve_empty_run_distance_far_east_km.unwrap_or(base);
                            format!("{base} км, {} {far} км", roads.join("/"))
                        }
                    }
                },
                r.foreign_railways.len(),
                r.foreign_exceptions.len(),
                r.deficit_railways.len(),
                r.deficit_export_max_distance_km,
                r.deficit_export_surcharge_rub,
                if r.gu12_check_enabled { "вкл." } else { "выкл." },
                match r.station_backlog_hard_days {
                    Some(hard) => format!(
                        "закрытие при Q ≥ {hard}×C, очередь ≤ {}×C без ожидания, штраф ожидания {:.0} руб./сут.",
                        r.station_backlog_soft_days, r.station_backlog_wait_penalty_rub_per_day,
                    ),
                    None => "выкл.".to_string(),
                },
                if r.convention_check_enabled { "вкл." } else { "выкл." },
                r.wash_procedure_cost_rub,
                r.empty_run_after_wash_cost_rub,
                if r.dirty_same_cargo_cap_enabled() {
                    format!("≤ {:.2} × промывочного маршрута", r.dirty_same_cargo_max_cost_ratio_to_wash)
                } else {
                    "выкл.".to_string()
                },
                if r.dirty_same_cargo_reward_share > 0.0 {
                    format!("{:.2} × (тариф до промывки + промывка)", r.dirty_same_cargo_reward_share)
                } else {
                    "выкл.".to_string()
                },
                r.repair_days_threshold,
                r.repair_days_threshold_foreign,
                r.foreign_washed_roads.len(),
                r.foreign_washed_picky_railways.len(),
                r.foreign_washed_picky_surcharge_rub,
                r.foreign_washed_picky_surplus_ratio,
                if r.p1_distance_adjust_enabled() {
                    format!(
                        "{:.1} руб./км × (км − {}), потолок {}, в отчёт не идёт",
                        r.p1_distance_rub_per_km,
                        r.p1_distance_neutral_km,
                        if r.p1_distance_cap_rub > 0.0 {
                            format!("±{:.0} руб.", r.p1_distance_cap_rub)
                        } else {
                            "нет".to_string()
                        },
                    )
                } else if p1_adjust_off_by_day1 {
                    "выкл. (режим --day1: без периода 10 не применяется)".to_string()
                } else {
                    "выкл.".to_string()
                },
            );
            r
        }
        Err(e) => {
            eprintln!("  business_rules.json: не загружен ({e}) — бизнес-правила 1–2 не применяются, потолок отстоя/путей выкл., правило 3 (ГУ-12) не выполняется без списка инотерриторий, правило 6 без поощрения, правило 7 без инотерриторий, правило 8 выключено");
            data::BusinessRules::default()
        }
    };

    // Правило 5: HASH telegrams_db → индекс действующих. Применяется в classify_pair,
    // отстое и ремонте. Нет пароля/Redis — fail-open, пустой индекс.
    let convention_index = data::load_conventions_at_startup(business_rules.convention_check_enabled);

    // Правило 3: потолок ГУ-12 на российский подсыл под погрузку (MSSQL SLP через gu12.py).
    // Спрос АПИ не режется: вагоны с инотерриторий закрывают узел без потолка.
    // Заявки не загрузились => потолок не ставится (громкое предупреждение).
    // Инотерритории — по дороге узла из ForeignRailways (правило 1); классификация по коду
    // станции ЕСР не используется как ненадёжная. Список пуст => проверку нельзя ограничить
    // территорией России => она не выполняется (в т.ч. если business_rules.json не загрузился).
    if business_rules.gu12_ready() {
        println!(
            "Дороги-инотерритории для ГУ-12 (ForeignRailways): {}",
            business_rules.foreign_railways.len()
        );
        match data::fetch_gu12_claims() {
            Ok(claims) => {
                let gu12_mode = gu12_mode_from_env();
                let st = data::apply_gu12_limits(
                    &mut demand_nodes,
                    &claims,
                    &business_rules.foreign_railways,
                    gu12_mode,
                );
                println!(
                    "Спрос с учётом ГУ-12 (правило 3, {}): узлов {} (спрос АПИ не режется); российский потолок {} ваг. из {}",
                    gu12_mode.label(), st.nodes_after, st.cars_after, st.cars_before,
                );
                println!(
                    "  заявок ГУ-12 согласованных: {} строк / {} станций (на станциях без спроса: {})",
                    st.claims_total, st.claim_stations, st.claims_without_demand,
                );
                println!(
                    "  по периодам 1..4, ваг.: было {:?} → стало {:?}",
                    st.cars_before_by_period, st.cars_after_by_period,
                );
                println!(
                    "  сопоставлено узлов: по ОКПО {}, по имени грузоотправителя {}, пропорционально по станции {}",
                    st.nodes_matched_okpo, st.nodes_matched_name, st.nodes_pool_only,
                );
                println!(
                    "  потолок ниже спроса АПИ: {} узлов / −{} ваг. российского подсыла; нулевой потолок (узел остаётся для вагонов с инотерриторий): {} узлов / {} ваг.; погрузка на инотерритории (без потолка): {} узлов / {} ваг.",
                    st.nodes_capped, st.cars_cut,
                    st.nodes_removed, st.cars_removed,
                    st.nodes_foreign, st.cars_foreign,
                );
            }
            Err(e) => eprintln!(
                "  [!] ГУ-12 (gu12.py json): не загружены ({e}) — спрос НЕ ограничен заявками ГУ-12, правило 3 не применено"
            ),
        }
    } else if business_rules.gu12_check_enabled {
        eprintln!(
            "  [!] ForeignRailways пуст — проверку ГУ-12 (правило 3) нельзя ограничить территорией России, она не выполняется"
        );
    }

    let mut supply_nodes = client.fetch_supply_nodes(&business_rules).await?;
    let supply1_total_cars: i32 = supply_nodes.iter()
                .map(|s| s.car_count)
                .sum();
    println!("Получено узлов предложения 1 сут.:  {} или {} вагонов", supply_nodes.len(), supply1_total_cars);

    // Сверка периодов по номерам: вагон, уже присутствующий в предложении АПИ (период 1,
    // Free и Assigned), из дислокации (период 10) исключается — иначе он участвовал бы в
    // оптимизации дважды. Приоритет у периода 1: это сегодняшняя дислокация, а не прогноз.
    // INCLUDE_PERIOD10=off (флаг run.sh --day1) — только 1-е сутки, к Redis/MSSQL не ходим.
    if !include_period10 {
        println!(
            "Дислокация 2-10 сут. (период 10): пропущена (INCLUDE_PERIOD10=off) — оптимизация только по вагонам 1-х суток"
        );
    } else {
        let period1_cars: HashSet<u64> = supply_nodes
            .iter()
            .flat_map(|s| s.car_numbers.iter().copied())
            .collect();
        match data::dislocations::fetch_dislocation_supply_nodes(&period1_cars, &business_rules) {
            Ok(disl) => {
                if disl.duplicates_within > 0 {
                    eprintln!(
                        "  [!] дислокация: {} повторов номеров внутри выгрузки dislocations.py — оставлено первое вхождение",
                        disl.duplicates_within,
                    );
                }
                if !disl.overlap_with_period1.is_empty() {
                    let n = disl.overlap_with_period1.len();
                    let sample: Vec<String> =
                        disl.overlap_with_period1.iter().take(10).map(|c| c.to_string()).collect();
                    eprintln!(
                        "  [!] дислокация: {n} вагонов периода 10 уже есть в предложении АПИ (период 1) — из периода 10 исключены, оставлены в периоде 1: {}{}",
                        sample.join(", "),
                        if n > sample.len() { ", …" } else { "" },
                    );
                }
                if !disl.nodes.is_empty() {
                    println!(
                        "  узлов дислокации (2-10 сут., период 10): {} или {} вагонов (в выгрузке {}, дублей {}, пересечений с периодом 1 {})",
                        disl.nodes.len(),
                        disl.cars_kept(),
                        disl.cars_total,
                        disl.duplicates_within,
                        disl.overlap_with_period1.len(),
                    );
                    supply_nodes.extend(disl.nodes);
                } else if disl.cars_total > 0 {
                    println!(
                        "  узлов дислокации (2-10 сут., период 10): 0 — все {} вагонов выгрузки уже в периоде 1 либо дубли",
                        disl.cars_total,
                    );
                }
            }
            Err(e) => eprintln!(
                "  дислокация 2-10 сут.: не загружена ({}), продолжаем только АПИ",
                e
            ),
        }
    }
    for (i, n) in supply_nodes.iter_mut().enumerate() {
        n.s_id = i + 1;
    }
    data::supply::apply_mass_unloading_flags(&mut supply_nodes);
    let supply_total_cars: i32 = supply_nodes.iter()
                .map(|s| s.car_count)
                .sum();
    println!("Получено узлов предложения всего:  {} или {} вагонов", supply_nodes.len(), supply_total_cars);

    // Разделяем по трём группам:
    //  1. Assigned  — уже назначены по факту, не участвуют в оптимизации.
    //  2. NeedsRepair — требуют ремонта, исключаются из оптимизации → «В ремонт».
    //  3. opt_supply  — свободные вагоны, участвуют в оптимизации.
    let (assigned_nodes, non_assigned): (Vec<_>, Vec<_>) = supply_nodes
        .iter()
        .cloned()
        .partition(|s| s.kind == CarKind::Assigned);

    let (repair_nodes, opt_supply): (Vec<_>, Vec<_>) = non_assigned
        .into_iter()
        .partition(|s| s.repair_status == RepairStatus::NeedsRepair);
    
    let [cars_free, cars_repair, cars_assigned] = [&opt_supply, &repair_nodes, &assigned_nodes]
    .map(|v| v.iter().map(|d| d.car_count).sum::<i32>());

    println!("  свободных для назначения:  {} или {} вагонов", opt_supply.len(), cars_free);
    println!("  требуют ремонта (В ремонт):{} или {} вагонов", repair_nodes.len(), cars_repair);
    println!("  по факту (Assigned):       {} или {} вагонов", assigned_nodes.len(), cars_assigned);

    let wash_codes = match data::load_wash_product_codes("data/references.json") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  WashProductCodes из references.json: не загружены ({e})");
            HashSet::new()
        }
    };
    // Правило 8: дороги иномойки — из business_rules.json (раньше NoCleaningRoads в references.json).
    let foreign_washed_roads = &business_rules.foreign_washed_roads;
    if !foreign_washed_roads.is_empty() {
        println!("Дороги иномойки (правило 8, ForeignWashedRoads): {}", foreign_washed_roads.len());
    }
    // Текущие коды ЕТСНГ «уже промыт/из ремонта» (WashedEmptyEtsngCodes): такие вагоны
    // считаются чистыми независимо от предыдущего груза (напр. 421208 — из промывки, 421195 — из ремонта).
    let washed_empty_codes = match data::load_washed_empty_codes("data/references.json") {
        Ok(c) => {
            println!("Коды «уже промыт/ремонт» (WashedEmptyEtsngCodes): {}", c.len());
            c
        }
        Err(e) => {
            eprintln!("  WashedEmptyEtsngCodes из references.json: не загружены ({e})");
            HashSet::new()
        }
    };
    // Ban-list «чужих» ёмкостей отстоя: фильтр БД отстоя по паре (код станции, ОКПО владельца).
    // Записи из справочника отбрасываются. Пустой ban-list (справочник не загружен) => фильтр отключён.
    let reserve_owners = match data::load_reserve_owners_banlist("data/reserve_owners.json") {
        Ok(s) => {
            println!("Ban-list чужих владельцев отстоя (reserve_owners.json): {} пар (станция+ОКПО)", s.len());
            s
        }
        Err(e) => {
            eprintln!("  reserve_owners.json не загружен ({e}) — фильтр отстоя по владельцам отключён");
            HashSet::new()
        }
    };
    let wash_stations = match data::wash::fetch_wash_stations() {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("  станции промывки (wash.py json): не загружены ({e})");
            vec![]
        }
    };
    let wash_demand_nodes = if wash_stations.is_empty() {
        Vec::new()
    } else {
        data::wash::wash_demand_nodes(&wash_stations, demand_nodes.len() + 1)
    };
    let wash_total_cap: i32 = wash_demand_nodes.iter()
                .map(|w| w.car_count)
                .sum();
    println!("Узлов спроса (промывка):     {} или мощность в период {} суток {} вагонов",
            wash_demand_nodes.len(),
            data::wash::PLANNING_HORIZON_DAYS,
            wash_total_cap);

    let mut demand_lp: Vec<DemandNode> = demand_nodes.clone();
    demand_lp.extend(wash_demand_nodes.clone());

    // Все вагоны с «грязным» ETSNG (без учёта правила 8 / ForeignWashedRoads).
    let n_supply_wash_raw = opt_supply
        .iter()
        .filter(|s| data::wash::supply_matches_wash_product_list(s, &wash_codes, &washed_empty_codes))
        .map(|s| s.car_count)
        .sum::<i32>();
    // Из них освобождены от промывки по дороге образования (правило 8).
    let n_supply_wash_exempt = opt_supply
        .iter()
        .filter(|s| {
            data::wash::supply_matches_wash_product_list(s, &wash_codes, &washed_empty_codes)
                && business_rules.is_foreign_washed(&s.railway_to)
        })
        .map(|s| s.car_count)
        .sum::<i32>();
    // Итого «грязных», требующих промывки.
    let n_supply_wash_list = n_supply_wash_raw - n_supply_wash_exempt;
    // По данным спроса Load (любая станция с тем же ЕТСНГ); без тарифа; иномойка — не считаем.
    let n_supply_wash_skip = opt_supply
        .iter()
        .filter(|s| {
            data::wash::supply_needs_wash(s, &wash_codes, foreign_washed_roads, &washed_empty_codes)
                && data::wash::load_demand_has_matching_dirty_etsng(s, &demand_nodes, foreign_washed_roads)
        })
        .map(|s| s.car_count)
        .sum::<i32>();
    println!(
        "  предложений с ЕТСНГ из списка промывки: {} вагонов (освобождены по правилу 8 / иномойка: {}; из них есть узел погрузки с тем же ЕТСНГ на любой станции — альтернатива промывке по спросу: {} вагонов)",
        n_supply_wash_list, n_supply_wash_exempt, n_supply_wash_skip
    );

    // -----------------------------------------------------------------------
    // 3. Получение тарифов
    //    stations_from: станции образования порожних opt_supply +
    //                   станции отправления Assigned-вагонов
    //    stations_to:   станции погрузки (demand) +
    //                   станции назначения Assigned-вагонов
    // -----------------------------------------------------------------------
    let stations_from: Vec<StationRef> = opt_supply
        .iter()
        .map(|s| (s.station_to_code.clone(), s.railway_to.clone()))
        .chain(
            // Берём первую (или единственную) дорогу/станцию отправления каждой группы.
            assigned_nodes.iter().flat_map(|s| {
                s.stations_from_code.iter()
                    .zip(s.railways_from.iter())
                    .take(1)
                    .map(|(code, rw)| (code.clone(), rw.clone()))
            })
        )
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|(code, rw)| StationRef::new(code, rw))
        .collect();

    let stations_to: Vec<StationRef> = demand_nodes
        .iter()
        .filter(|d| d.purpose == DemandPurpose::Load)
        .map(|d| (d.station_code.clone(), d.railway_name.clone()))
        .chain(
            // Добавляем станции фактического назначения Assigned-вагонов.
            assigned_nodes.iter()
                .map(|s| (s.station_to_code.clone(), s.railway_to.clone()))
        )
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|(code, rw)| StationRef::new(code, rw))
        .collect();

    let tariff_nodes = client.fetch_tariffs(&stations_from, &stations_to).await?;
    println!("Получено тарифов:            {}", tariff_nodes.len());

    // -----------------------------------------------------------------------
    // 3а. Тарифы до ремонтных станций (data/repairs.json)
    //     stations_from: текущие станции ремонтных вагонов (repair_nodes)
    //     stations_to:   все ремонтные станции из словаря
    //     Assigned-вагоны сохраняют исходные назначения, в расчёт не входят.
    // -----------------------------------------------------------------------
    let repair_stations = match data::load_repair_stations("data/repairs.json") {
        Ok(rs) if !rs.is_empty() => {
            println!("Загружено ремонтных станций: {}", rs.len());
            rs
        }
        Ok(_) => {
            eprintln!("  data/repairs.json пуст; ремонтный маршрут не будет выбран");
            vec![]
        }
        Err(e) => {
            eprintln!("  data/repairs.json не загружен ({}); ремонтный маршрут не будет выбран", e);
            vec![]
        }
    };

    let repair_tariffs = if !repair_stations.is_empty() && !repair_nodes.is_empty() {
        let repair_from: Vec<StationRef> = repair_nodes
            .iter()
            .map(|s| (s.station_to_code.clone(), s.railway_to.clone()))
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|(code, rw)| StationRef::new(code, rw))
            .collect();

        let repair_to: Vec<StationRef> = repair_stations
            .iter()
            .map(|rs| (rs.station_code.clone(), rs.railway.clone()))
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|(code, rw)| StationRef::new(code, rw))
            .collect();

        match client.fetch_tariffs(&repair_from, &repair_to).await {
            Ok(t) => {
                println!("Тарифов до ремонтных ст.:    {}", t.len());
                t
            }
            Err(e) => {
                eprintln!("  тарифы до ремонтных станций: не загружены ({})", e);
                vec![]
            }
        }
    } else {
        vec![]
    };

    // -----------------------------------------------------------------------
    // 3б. Тарифы до станций промывки + надбавка правила 6 (промывка + порожний пробег
    //     до погрузки, WashProcedureCostRub + EmptyRunAfterWashCostRub).
    //     В LP используется только суммарная стоимость дуги «до промывки».
    //     FrETSNGCode: груженый — текущий груз, порожний — PrevFrETSNG (доминирующий в группе).
    // -----------------------------------------------------------------------
    let wash_station_refs = data::wash::wash_station_refs(&wash_stations);
    let mut wash_tariff_map: HashMap<(String, String), TariffNode> = HashMap::new();
    if !wash_station_refs.is_empty() {
        let wash_from: Vec<StationRef> = opt_supply
            .iter()
            .filter(|s| data::wash::supply_needs_wash(s, &wash_codes, foreign_washed_roads, &washed_empty_codes))
            .map(|s| (s.station_to_code.clone(), s.railway_to.clone()))
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|(code, rw)| StationRef::new(code, rw))
            .collect();

        if !wash_from.is_empty() {
            match client.fetch_tariffs(&wash_from, &wash_station_refs).await {
                Ok(items) => {
                    for mut t in items {
                        t.cost += business_rules.wash_path_surcharge_rub();
                        wash_tariff_map.insert(
                            (t.station_from_code.clone(), t.station_to_code.clone()),
                            t,
                        );
                    }
                }
                Err(e) => eprintln!("  тарифы до промывки: {}", e),
            }
            println!(
                "Тарифов до промывки (с надбавкой {}+{}={} руб.): {}",
                business_rules.wash_procedure_cost_rub as i64,
                business_rules.empty_run_after_wash_cost_rub as i64,
                business_rules.wash_path_surcharge_rub() as i64,
                wash_tariff_map.len(),
            );
        }
    } else if !wash_codes.is_empty() && wash_stations.is_empty() {
        println!("Тарифы до промывки:         не запрошены (нет станций промывки)");
    }

    // -----------------------------------------------------------------------
    // 3в. Ограничения ДМЗИ: лимиты подсыла порожних вагонов на дороги.
    //     Период 1 — сумма Normativ за сутки 1–5; период 10 — за весь горизонт 7 суток.
    //     Недоступность АПИ не блокирует прогон: квоты просто не применяются.
    // -----------------------------------------------------------------------
    let dmzi_limits: Option<solver::DmziLimits> = match client.fetch_dmzi_quotas().await {
        Ok(q) if !q.is_empty() => {
            let mut railways: Vec<_> = q.by_railway.iter().collect();
            railways.sort_by(|a, b| a.0.cmp(b.0));
            println!(
                "Ограничения ДМЗИ:            {} дорог ({} записей Ostatok)",
                railways.len(),
                q.records,
            );
            for (rw, quota) in &railways {
                println!(
                    "  {:4} период 1 ≤ {:>4}, период 10 ≤ {:>4} ваг.",
                    rw, quota.limit_p1, quota.limit_p10,
                );
            }
            Some(q.to_limits())
        }
        Ok(_) => {
            eprintln!(
                "  ВНИМАНИЕ: ответ ДМЗИ пуст — прогон выполняется БЕЗ ограничений ДМЗИ!"
            );
            None
        }
        Err(e) => {
            eprintln!(
                "  ВНИМАНИЕ: ДМЗИ недоступно ({e}) — прогон выполняется БЕЗ ограничений ДМЗИ!"
            );
            None
        }
    };

    // -----------------------------------------------------------------------
    // 3г. Узлы отстоя (резервы): ёмкости для излишка порожних вагонов.
    //     Накопительная SQLite-БД: при каждом прогоне свежие разрешения из АПИ
    //     складываются в БД (upsert по etran_id), а узлы строятся уже из БД с
    //     фильтром по сроку действия (date_beg/date_end). Так разрешения,
    //     действующие месяцами, не теряются при сбое АПИ в отдельные сутки.
    //     Назначаются вторым этапом после основного решения — не конкурируют
    //     с заявками клиентов. Недоступность АПИ/БД не блокирует прогон.
    // -----------------------------------------------------------------------
    let reserve_data: Option<data::ReserveData> = match data::open_reserves_db(data::reserves_db_path()) {
        Ok(conn) => {
            match data::sync_reserves_to_db(&client, &conn).await {
                Ok(stats) => println!(
                    "БД отстоя обновлена:         получено {} / записано {} / без etran_id {}",
                    stats.fetched, stats.upserted, stats.skipped_no_etran,
                ),
                Err(e) => eprintln!(
                    "  ВНИМАНИЕ: обновление БД отстоя не удалось ({e}) — используем ранее накопленные данные"
                ),
            }
            match data::load_active_reserve_nodes(&conn, chrono::Utc::now().date_naive(), &reserve_owners) {
                Ok(r) if !r.nodes.is_empty() => {
                    println!(
                        "Узлы отстоя (резервы):       {} узлов / ёмкость {} ваг. \
                         (записей в БД {}, дублей {}, отфильтровано {}, чужих по ban-list {})",
                        r.nodes.len(),
                        r.total_capacity(),
                        r.raw_records,
                        r.duplicates,
                        r.filtered,
                        r.foreign_filtered,
                    );
                    Some(r)
                }
                Ok(_) => {
                    eprintln!("  ВНИМАНИЕ: в БД отстоя нет активных разрешений — излишек не будет назначен в отстой");
                    None
                }
                Err(e) => {
                    eprintln!(
                        "  ВНИМАНИЕ: чтение БД отстоя не удалось ({e}) — излишек не будет назначен в отстой"
                    );
                    None
                }
            }
        }
        Err(e) => {
            eprintln!(
                "  ВНИМАНИЕ: БД отстоя недоступна ({e}) — излишек не будет назначен в отстой"
            );
            None
        }
    };

    // -----------------------------------------------------------------------
    // Справочник свободных ёмкостей подъездных путей крупных станций погрузки.
    //     Пересобирается каждый суточный прогон: load_stations.json (ёмкость путей) +
    //     MSSQL (станции погрузки за 6 мес. из MSSQL_DB_SLP + вагоны на станции из
    //     MSSQL_DB_ASUVP). Итог → data/load_stations_free_capacity.json. Будет
    //     использован для размещения невостребованных вагонов (следующая задача).
    //     Недоступность БД не блокирует прогон.
    // -----------------------------------------------------------------------
    let free_loadroads: Vec<data::FreeLoadRoad> = match data::build_free_loadroads(
        data::free_loadroads::DEFAULT_LOAD_STATIONS_PATH,
        data::free_loadroads::DEFAULT_OUTPUT_PATH,
    ) {
        Ok(records) => {
            println!(
                "Свободные ёмкости путей:     {} крупных станций -> {}",
                records.len(),
                data::free_loadroads::DEFAULT_OUTPUT_PATH,
            );
            records
        }
        Err(e) => {
            eprintln!("  ВНИМАНИЕ: справочник свободных ёмкостей путей не построен ({e})");
            Vec::new()
        }
    };

    // -----------------------------------------------------------------------
    // Правило 4: загруженность станций погрузки. Q — CarsOnStation из АПИ спроса,
    //     (по грузоотправителю, суммируется по станции), C — мощность погрузки из
    //     data/load_stations.json. Q ≥ K_hard·C → станция
    //     закрыта во все периоды; ниже порога вагон, приезжающий раньше рассасывания
    //     очереди, ждёт (сдвиг суток погрузки + штраф). Справочник не загрузился =>
    //     правило не применяется (громкое предупреждение).
    // -----------------------------------------------------------------------
    let station_backlog = if business_rules.station_backlog_enabled() {
        match data::StationBacklogIndex::load_and_build(
            data::station_backlog::DEFAULT_LOAD_STATIONS_PATH,
            &demand_lp,
            &business_rules,
        ) {
            Ok(idx) => {
                let st = &idx.stats;
                println!(
                    "Загруженность станций (правило 4): мощность известна у {} станций справочника; станций спроса {}, проверено {}, без мощности {}",
                    st.capacity_stations, st.demand_stations, st.checked_stations, st.unknown_capacity_stations,
                );
                println!(
                    "  закрыто станций: {} ({} узлов / {} ваг. спроса); с очередью (ожидание подсыла): {}; станций с несколькими грузоотправителями: {}; грузоотправителей с разным CarsOnStation по узлам: {}",
                    st.closed_stations, st.closed_demand_nodes, st.closed_demand_cars,
                    st.waiting_stations, st.multi_sender_stations, st.inconsistent_q_senders,
                );
                for (name, railway, q, c) in st.closed_list.iter().take(10) {
                    println!(
                        "    · закрыта {name} ({railway}): на станции {q} ваг., мощность {c} ваг./сут. ({:.1} сут. работы)",
                        *q as f64 / (*c).max(1) as f64,
                    );
                }
                if st.closed_list.len() > 10 {
                    println!("    · ...ещё {} закрытых станций", st.closed_list.len() - 10);
                }
                for (name, railway, q, c, t) in st.waiting_list.iter().take(10) {
                    println!(
                        "    · очередь {name} ({railway}): на станции {q} ваг., мощность {c} ваг./сут. — подсыл не раньше {t}-х суток",
                    );
                }
                if st.waiting_list.len() > 10 {
                    println!("    · ...ещё {} станций с очередью", st.waiting_list.len() - 10);
                }
                idx
            }
            Err(e) => {
                eprintln!(
                    "  [!] Загруженность станций (правило 4): справочник не загружен ({e}) — правило не применяется"
                );
                data::StationBacklogIndex::disabled()
            }
        }
    } else {
        data::StationBacklogIndex::disabled()
    };

    // -----------------------------------------------------------------------
    // 4. Построение дуг транспортной задачи
    // -----------------------------------------------------------------------
    let (arcs, arc_stats) = solver::build_task_arcs(
        &opt_supply,
        &demand_lp,
        &tariff_nodes,
        &wash_codes,
        &washed_empty_codes,
        &wash_tariff_map,
        &business_rules,
        &station_backlog,
        &convention_index,
    );

    let total = arc_stats.total_pairs;
    println!("Всего пар supply×demand:     {}", total);
    println!(
        "  без тарифа:                        {} ({:.1}%)",
        arc_stats.no_tariff,
        100.0 * arc_stats.no_tariff as f64 / total.max(1) as f64,
    );
    println!(
        "  нарушение срока (жёстко):          {} ({:.1}%)",
        arc_stats.bad_period,
        100.0 * arc_stats.bad_period as f64 / total.max(1) as f64,
    );
    println!(
        "  несовм. тип вагона:                {} ({:.1}%)",
        arc_stats.bad_type,
        100.0 * arc_stats.bad_type as f64 / total.max(1) as f64,
    );
    println!(
        "  грязный вагон → чужой ЕТСНГ (правило 6): {} ({:.1}%)",
        arc_stats.dirty_etsng_mismatch,
        100.0 * arc_stats.dirty_etsng_mismatch as f64 / total.max(1) as f64,
    );
    println!(
        "  грязный: погрузка дороже промывочного маршрута → в промывку (правило 6): {} ({:.1}%)",
        arc_stats.dirty_far_prefer_wash,
        100.0 * arc_stats.dirty_far_prefer_wash as f64 / total.max(1) as f64,
    );
    println!(
        "  погрузка дальше потолка расстояния ({}): {} ({:.1}%)",
        business_rules
            .max_empty_run_distance_km
            .map(|km| format!("{km} км"))
            .unwrap_or_else(|| "выкл.".to_string()),
        arc_stats.too_far,
        100.0 * arc_stats.too_far as f64 / total.max(1) as f64,
    );
    println!(
        "  инотерритория (правило 1):           {} ({:.1}%)",
        arc_stats.foreign_territory,
        100.0 * arc_stats.foreign_territory as f64 / total.max(1) as f64,
    );
    println!(
        "  вывоз с дефицитной дороги (правило 2): {} ({:.1}%)",
        arc_stats.deficit_export,
        100.0 * arc_stats.deficit_export as f64 / total.max(1) as f64,
    );
    println!(
        "  станция закрыта очередью (правило 4): {} ({:.1}%)",
        arc_stats.station_overloaded,
        100.0 * arc_stats.station_overloaded as f64 / total.max(1) as f64,
    );
    println!(
        "  конвенция РЖД (правило 5):           {} ({:.1}%)",
        arc_stats.convention_ban,
        100.0 * arc_stats.convention_ban as f64 / total.max(1) as f64,
    );
    if !arc_stats.convention_by_number.is_empty() {
        let mut nums: Vec<(&String, &usize)> = arc_stats.convention_by_number.iter().collect();
        nums.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let top: Vec<String> = nums
            .iter()
            .take(10)
            .map(|(n, c)| format!("№{n} ({c})"))
            .collect();
        println!("    · телеграммы: {}", top.join(", "));
        if nums.len() > 10 {
            println!("    · ...ещё {} номеров", nums.len() - 10);
        }
    }
    println!(
        "  допустимых дуг с надбавкой по правилам: {} ({:.1}%)",
        arc_stats.arcs_rule_surcharged,
        100.0 * arc_stats.arcs_rule_surcharged as f64 / total.max(1) as f64,
    );
    if arc_stats.arcs_foreign_washed_picky > 0 {
        println!(
            "    · из них иномойка → капризная дорога (правило 8, профицит): {} ({:.1}%)",
            arc_stats.arcs_foreign_washed_picky,
            100.0 * arc_stats.arcs_foreign_washed_picky as f64 / total.max(1) as f64,
        );
    }
    println!(
        "  допустимых дуг с ожиданием в очереди станции (правило 4): {} ({:.1}%)",
        arc_stats.arcs_backlog_wait,
        100.0 * arc_stats.arcs_backlog_wait as f64 / total.max(1) as f64,
    );
    if arc_stats.arcs_dirty_rewarded > 0 {
        println!(
            "  допустимых дуг «грязный → свой груз» с поощрением (правило 6): {} ({:.1}%), в среднем {:.0} руб./дугу",
            arc_stats.arcs_dirty_rewarded,
            100.0 * arc_stats.arcs_dirty_rewarded as f64 / total.max(1) as f64,
            arc_stats.dirty_reward_total_rub / arc_stats.arcs_dirty_rewarded as f64,
        );
    }
    if arc_stats.gu12_blocked > 0 {
        println!(
            "  пар погрузки с российских дорог не построено: нулевой потолок ГУ-12 ({})",
            arc_stats.gu12_blocked,
        );
    }
    if business_rules.p1_distance_adjust_enabled() {
        let bonus_avg = if arc_stats.arcs_p1_distance_bonus > 0 {
            arc_stats.p1_distance_bonus_total_rub / arc_stats.arcs_p1_distance_bonus as f64
        } else {
            0.0
        };
        let surcharge_avg = if arc_stats.arcs_p1_distance_surcharge > 0 {
            arc_stats.p1_distance_surcharge_total_rub / arc_stats.arcs_p1_distance_surcharge as f64
        } else {
            0.0
        };
        println!(
            "  поправка периода 1 по расстоянию: бонус на {} дугах ({:.1}%, в среднем −{:.0} руб.), надбавка на {} дугах ({:.1}%, в среднем +{:.0} руб.)",
            arc_stats.arcs_p1_distance_bonus,
            100.0 * arc_stats.arcs_p1_distance_bonus as f64 / total.max(1) as f64,
            bonus_avg,
            arc_stats.arcs_p1_distance_surcharge,
            100.0 * arc_stats.arcs_p1_distance_surcharge as f64 / total.max(1) as f64,
            surcharge_avg,
        );
    }
    println!(
        "  допустимых дуг со штрафом за срок:   {} ({:.1}%)",
        arc_stats.arcs_period_penalized,
        100.0 * arc_stats.arcs_period_penalized as f64 / total.max(1) as f64,
    );
    println!(
        "  допустимых дуг всего в LP:      {} ({:.1}%)",
        arc_stats.feasible,
        100.0 * arc_stats.feasible as f64 / total.max(1) as f64,
    );

    // --- Статистика ограничений минимальной партии (MIN_BATCH) по классам пар ---
    // Маршрутные дуги отличаем по порогу (B=10); остальные по узлу предложения:
    // массовая выгрузка ↔ средняя станция.
    {
        use std::collections::HashSet;
        let mut mass_arcs = 0_usize;
        let mut mid_arcs = 0_usize;
        let mut route_arcs = 0_usize;
        let mut mass_pairs: HashSet<(&str, &str)> = HashSet::new();
        let mut mid_pairs: HashSet<(&str, &str)> = HashSet::new();
        let mut route_pairs: HashSet<(&str, &str)> = HashSet::new();
        let mut mid_supply_stations: HashSet<&str> = HashSet::new();
        let mut mid_demand_stations: HashSet<&str> = HashSet::new();
        let mut route_supply_stations: HashSet<&str> = HashSet::new();
        let mut route_demand_stations: HashSet<&str> = HashSet::new();
        for arc in arcs.iter().filter(|a| a.has_pair_min_batch()) {
            let pair = (arc.supply_station_code.as_str(), arc.demand_station_code.as_str());
            if arc.pair_min_batch == solver::MIN_BATCH_TO_ROUTE_DEMAND_STATION {
                route_arcs += 1;
                route_pairs.insert(pair);
                route_supply_stations.insert(pair.0);
                route_demand_stations.insert(pair.1);
            } else if opt_supply[arc.s_idx].is_mass_unloading {
                mass_arcs += 1;
                mass_pairs.insert(pair);
            } else {
                mid_arcs += 1;
                mid_pairs.insert(pair);
                mid_supply_stations.insert(pair.0);
                mid_demand_stations.insert(pair.1);
            }
        }
        println!(
            "Ограничения партии (MIN_BATCH): массовая выгрузка — {} дуг / {} пар; \
             средние станции — {} дуг / {} пар (станций образования: {}, погрузки: {})",
            mass_arcs,
            mass_pairs.len(),
            mid_arcs,
            mid_pairs.len(),
            mid_supply_stations.len(),
            mid_demand_stations.len(),
        );
        println!(
            "  маршрутные отправки (B={}): {} дуг / {} пар (станций образования: {}, маршрутных станций погрузки: {})",
            solver::MIN_BATCH_TO_ROUTE_DEMAND_STATION,
            route_arcs,
            route_pairs.len(),
            route_supply_stations.len(),
            route_demand_stations.len(),
        );
    }

    // -----------------------------------------------------------------------
    // 5. Анализ баланса и начальное жадное решение
    // -----------------------------------------------------------------------
    solver::print_balance(&opt_supply, &demand_lp);
    if business_rules.market_surplus(&opt_supply, &demand_lp) {
        let mut picky: Vec<&str> = business_rules
            .foreign_washed_picky_railways
            .iter()
            .map(String::as_str)
            .collect();
        picky.sort_unstable();
        println!(
            "Правило 8: профицит порожних (предложение > {:.2} × спрос погрузки) — надбавка {:.0} руб. на подсыл иномойки на капризные дороги ({})",
            business_rules.foreign_washed_picky_surplus_ratio,
            business_rules.foreign_washed_picky_surcharge_rub,
            picky.join(", "),
        );
    }

    // Штрафы за остаток предложения по узлам (общие для MIP, ALNS и выбора seed).
    // При дефиците грязные узлы (есть Wash-дуги) штрафуются PENALTY_EXCESS_DIRTY —
    // иначе MIP оставляет их в остатке (промывка ничего «не закрывает» в модели),
    // и грязные вагоны уезжают в отстой при незакрытом спросе.
    let excess_penalties = solver::ExcessPenalties::build(&arcs, &opt_supply, &demand_lp);
    if excess_penalties.deficit {
        println!(
            "Штраф за остаток грязных вагонов (дефицит): {:.0} руб./ваг. вместо {:.0} — узлов {}, вагонов {} (промывка выгоднее отстоя)",
            solver::PENALTY_EXCESS_DIRTY,
            solver::PENALTY_EXCESS,
            excess_penalties.dirty_nodes,
            excess_penalties.dirty_cars,
        );
    } else {
        println!(
            "Штраф за остаток предложения: {:.0} руб./ваг. для всех узлов (профицит — грязные вагоны в отстой без промывки)",
            solver::PENALTY_EXCESS,
        );
    }

    let greedy_result =
        solver::greedy_initial_solution(&arcs, &opt_supply, &demand_lp, dmzi_limits.as_ref());
    solver::print_greedy_result(&greedy_result, &opt_supply, &demand_lp);

    // -----------------------------------------------------------------------
    // 6. MIP-решение (HiGHS branch-and-cut).
    //    По умолчанию используется warm-start из greedy (с санацией пар, где
    //    нарушен MIN_BATCH). Отключить можно переменной окружения
    //    `MIP_WARM_START=off` — тогда HiGHS строит решение «с нуля» через
    //    LP-relaxation. Это режим для бенчмарка: сравнить время и качество.
    //    Формулировка big-M (бинарные y_pair) — см. src/solver/mip.rs.
    // -----------------------------------------------------------------------
    let warm_start_enabled = std::env::var("MIP_WARM_START")
        .map(|v| {
            let v = v.trim().to_lowercase();
            !matches!(v.as_str(), "off" | "0" | "false" | "no" | "none")
        })
        .unwrap_or(true);
    let warm_start_vec = if warm_start_enabled {
        Some(solver::greedy_to_arc_vals(&greedy_result, arcs.len()))
    } else {
        None
    };
    println!(
        "MIP warm-start: {} (управляется env MIP_WARM_START={{on|off}}, по умолч. on)",
        if warm_start_enabled { "ON (greedy)" } else { "OFF (HiGHS с нуля)" }
    );

    let mip_t0 = std::time::Instant::now();
    let mip_outcome = solver::solve_mip(
        &arcs,
        &opt_supply,
        &demand_lp,
        solver::DEFAULT_MIP_TIME_LIMIT,
        warm_start_vec.as_deref(),
        None, // rel_gap — берём DEFAULT_MIP_REL_GAP
        None, // pair_min_batch_override — для главного MIP не нужен
        dmzi_limits.as_ref(),
        &excess_penalties,
    );
    let mip_elapsed = mip_t0.elapsed();
    solver::print_mip_result(&mip_outcome.optim, &opt_supply, &demand_lp);
    println!(
        "MIP время: {:.2} сек (warm-start: {})",
        mip_elapsed.as_secs_f64(),
        if warm_start_enabled { "ON" } else { "OFF" },
    );

    // --- Диагностика MIP: сырой статус HiGHS, gap и покрытие ---
    // Помогает понять, почему MIP оставляет вагоны нераспределёнными: сразу видно,
    //   а) остановился ли HiGHS по gap / по time_limit / доказал оптимум,
    //   б) сколько неиспользованного предложения и неудовлетворённого Load-спроса
    //      в итоговом инкумбенте,
    //   в) как это соотносится с greedy — иначе приходится гадать по логам ALNS.
    {
        let mip_undist = mip_outcome.optim.penalty_cars as i32 + mip_outcome.optim.excess_supply as i32;
        let greedy_undist = greedy_result.unmet_demand + greedy_result.excess_supply;
        println!(
            "MIP диагностика: status={:?}, gap={:.4}%, undist={} (unmet={}, excess={}), real_cost={:.2}",
            mip_outcome.status,
            mip_outcome.mip_gap * 100.0,
            mip_undist,
            mip_outcome.optim.penalty_cars as i32,
            mip_outcome.optim.excess_supply as i32,
            mip_outcome.optim.total_cost,
        );
        println!(
            "MIP vs greedy:   greedy undist={} (unmet={}, excess={}), real_cost={:.2}  →  Δundist={:+}, Δcost={:+.2}",
            greedy_undist,
            greedy_result.unmet_demand,
            greedy_result.excess_supply,
            greedy_result.total_cost,
            mip_undist - greedy_undist,
            mip_outcome.optim.total_cost - greedy_result.total_cost,
        );
        // Предупреждаем только если MIP оставил реальный спрос незакрытым (unmet > 0).
        // Сравнение по undist намеренно убрано: после разделения PENALTY_EXCESS/PENALTY_UNMET
        // MIP правомерно имеет больший excess (отстой дёшев), но меньший real_cost —
        // это корректное поведение, а не дефект.
        let mip_unmet = mip_outcome.optim.penalty_cars as i32;
        if mip_unmet > 0 {
            println!(
                "  ВНИМАНИЕ: MIP не закрыл {} ваг. спроса на погрузку (unmet > 0). Возможные причины:\n           (а) PENALTY_UNMET ниже стоимости единственно допустимых плеч,\n           (б) HiGHS остановился по rel_gap до закрытия всего спроса.",
                mip_unmet,
            );
        }
    }

    // --- Поштучная диагностика по узлам с excess_supply ---
    // Перечисляем конкретные узлы, которые MIP оставил нераспределёнными, и
    // классифицируем причину (нет дуг / MIN_BATCH-тупик / штраф < дуги / …).
    // Это то, что нужно, чтобы понять: релаксировать ли MIN_BATCH, или проблема
    // в структуре входных данных (тарифы, тип вагона, дорога).
    if mip_outcome.optim.excess_supply as i32 > 0 {
        solver::diagnose_excess_supply(
            &arcs,
            &mip_outcome.arc_vals,
            &opt_supply,
            &demand_lp,
            dmzi_limits.as_ref(),
        );
    }

    // -----------------------------------------------------------------------
    // 7. ALNS-оптимизация — только если MIP не нашёл глобальный оптимум.
    //    При `is_globally_optimal() == true` HiGHS гарантирует оптимальность
    //    в рамках допустимого разрыва, и запускать ALNS — пустая потеря времени.
    // -----------------------------------------------------------------------
    let (optim_result, solution, remaining_supply_vec) = if mip_outcome.is_globally_optimal() {
        println!(
            "MIP нашёл глобальный оптимум (gap={:.4}%) — фаза ALNS пропущена.",
            mip_outcome.mip_gap * 100.0
        );

        // Восстанавливаем остатки предложения напрямую из MIP-решения:
        // остатки нужны для post-processing разбивки по периодам поставки.
        let mut rem: Vec<i32> = opt_supply.iter().map(|s| s.car_count).collect();
        for (arc, &q) in arcs.iter().zip(mip_outcome.arc_vals.iter()) {
            rem[arc.s_idx] -= q.round() as i32;
        }
        (mip_outcome.optim.clone(), mip_outcome.arc_vals.clone(), rem)
    } else {
        // Берём лучший из (greedy, MIP) как старт ALNS. Определение «лучше»
        // совпадает с accept-критерием ALNS (`solver::alns::accept_candidate`) —
        // иначе ALNS сразу же мог бы «откатить» seed:
        //   1) меньше unmet (неудовлетворённый Load-спрос) — жёсткий приоритет;
        //   2) при равенстве — ниже полная целевая функция
        //      objective = real_cost + PENALTY_UNMET·unmet + PENALTY_EXCESS·excess.
        //
        // Важно: «undist = unmet + excess» здесь НЕ используется как критерий.
        // После введения PENALTY_EXCESS << PENALTY_UNMET, MIP правомерно оставляет
        // больше вагонов в excess (отстой дёшев), поэтому его undist > greedy.undist
        // — это не дефект, а оптимальное поведение. Критерий min(undist) ошибочно
        // выбирал бы greedy, игнорируя существенно меньшую реальную стоимость MIP.
        let mip_as_greedy = solver::arc_vals_to_greedy_result(
            &mip_outcome.arc_vals, &arcs, &opt_supply, &demand_lp,
        );
        let greedy_undist = greedy_result.unmet_demand + greedy_result.excess_supply;
        let mip_undist    = mip_as_greedy.unmet_demand + mip_as_greedy.excess_supply;

        // Кортежи для лексикографического сравнения; objective округляем до рубля.
        let greedy_obj = greedy_result.objective_cost(&opt_supply, &excess_penalties);
        let mip_obj    = mip_as_greedy.objective_cost(&opt_supply, &excess_penalties);
        let greedy_key = (greedy_result.unmet_demand, greedy_obj as i64);
        let mip_key    = (mip_as_greedy.unmet_demand, mip_obj as i64);

        let alns_seed = if mip_key < greedy_key { &mip_as_greedy } else { &greedy_result };
        let seed_name = if mip_key < greedy_key { "MIP" } else { "greedy" };

        println!("--- SEED ДЛЯ ALNS ---");
        println!(
            "  greedy : undist {:>4} (unmet {:>3} + excess {:>3}), assigned {:>4}, real_cost {:>12.2} руб., objective {:>12.2} руб.",
            greedy_undist, greedy_result.unmet_demand, greedy_result.excess_supply,
            greedy_result.assigned_cars, greedy_result.total_cost, greedy_obj,
        );
        println!(
            "  MIP    : undist {:>4} (unmet {:>3} + excess {:>3}), assigned {:>4}, real_cost {:>12.2} руб., objective {:>12.2} руб.",
            mip_undist, mip_as_greedy.unmet_demand, mip_as_greedy.excess_supply,
            mip_as_greedy.assigned_cars, mip_as_greedy.total_cost, mip_obj,
        );
        println!(
            "  выбран : {} (критерий: min(unmet), затем min(objective) — как accept ALNS)",
            seed_name,
        );
        println!("---------------------");

        let alns_config = solver::AlnsConfig::default();
        let alns_result = solver::run_alns(
            alns_seed, &arcs, &opt_supply, &demand_lp, &alns_config,
            dmzi_limits.as_ref(),
        );
        let optim_result = alns_result.to_optim_result(&demand_lp);
        let solution     = alns_result.arc_vals.clone();
        let rem          = alns_result.best_state.remaining_supply.clone();
        (optim_result, solution, rem)
    };

    // --- Диагностика незакрытого Load-спроса (зеркально diagnose_excess_supply) ---
    // Показывает, какие заявки структурно недостижимы (нет дуг — нужны новые
    // тарифы/смягчение фильтров), а какие потенциально закрываемы (партия / ДМЗИ /
    // конкуренция за предложение). Помогает понять реальный потолок покрытия.
    if optim_result.penalty_cars as i32 > 0 {
        solver::diagnose_unmet_demand(
            &arcs,
            &solution,
            &opt_supply,
            &demand_lp,
            &tariff_nodes,
            &wash_codes,
            &washed_empty_codes,
            &wash_tariff_map,
            dmzi_limits.as_ref(),
            &business_rules,
            &station_backlog,
            &convention_index,
        );
    }

    // --- Утилизация квот ДМЗИ финальным решением ---
    if let Some(limits) = &dmzi_limits {
        let idx = solver::DmziIndex::build(&arcs, &opt_supply, &demand_lp, limits);
        let used = idx.usage_from_arc_vals(&solution);
        println!("--- УТИЛИЗАЦИЯ КВОТ ДМЗИ ---");
        let mut violated = 0_usize;
        for (b, ((rw, period), limit)) in idx.buckets.iter().enumerate() {
            if used[b] == 0 && *limit == 0 {
                continue;
            }
            let mark = if used[b] > *limit { "  [!] ПРЕВЫШЕНИЕ" } else { "" };
            if used[b] > *limit {
                violated += 1;
            }
            println!(
                "  {:4} период {:>2}: {:>4} из {:>4} ваг.{}",
                rw, period, used[b], limit, mark,
            );
        }
        if violated > 0 {
            eprintln!(
                "  ВНИМАНИЕ: квоты ДМЗИ превышены в {} бакетах — проверьте решатель!",
                violated,
            );
        }
        println!("----------------------------");
    }

    let mut remaining_supply_p1 = 0_i32;
    let mut remaining_supply_p10 = 0_i32;
    for (s, &rem) in opt_supply.iter().zip(remaining_supply_vec.iter()) {
        if rem <= 0 {
            continue;
        }
        match s.supply_period {
            1 => remaining_supply_p1 += rem,
            10 => remaining_supply_p10 += rem,
            _ => {}
        }
    }
    let remaining_supply_other = (optim_result.excess_supply as i32
        - remaining_supply_p1
        - remaining_supply_p10)
        .max(0);

    // -----------------------------------------------------------------------
    // 6а. Этап 2: размещение излишка в узлы отстоя (резервы).
    //     Тарифы запрашиваются от станций излишка (станции дислокации
    //     порожних supply-узлов) к станциям резервов. ДМЗИ не расходуется.
    //     Пары дальше MaxReserveEmptyRunDistanceKm (ДВС/ЗАБ — FarEast) не строятся.
    // -----------------------------------------------------------------------
    let mut reserve_assignments: Vec<solver::ReserveAssignment> = Vec::new();
    let reserve_nodes: Vec<ReserveNode> = reserve_data
        .map(|r| r.nodes)
        .unwrap_or_default();
    let total_excess: i32 = remaining_supply_vec.iter().map(|&r| r.max(0)).sum();
    if !reserve_nodes.is_empty() && total_excess > 0 {
        let excess_from: Vec<StationRef> = opt_supply
            .iter()
            .zip(remaining_supply_vec.iter())
            .filter(|&(_, &rem)| rem > 0)
            .map(|(s, _)| (s.station_to_code.clone(), s.railway_to.clone()))
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|(code, rw)| StationRef::new(code, rw))
            .collect();
        let reserve_refs = data::reserve_station_refs(&reserve_nodes);

        match client.fetch_tariffs(&excess_from, &reserve_refs).await {
            Ok(items) => {
                let reserve_tariff_map: HashMap<(String, String), TariffNode> = items
                    .into_iter()
                    .map(|t| ((t.station_from_code.clone(), t.station_to_code.clone()), t))
                    .collect();
                println!(
                    "Тарифов до отстоя:           {} (станций излишка {}, станций отстоя {})",
                    reserve_tariff_map.len(),
                    excess_from.len(),
                    reserve_refs.len(),
                );
                reserve_assignments = solver::solve_reserve_assignment(
                    &remaining_supply_vec,
                    &opt_supply,
                    &reserve_nodes,
                    &reserve_tariff_map,
                    &convention_index,
                    &business_rules,
                );
            }
            Err(e) => eprintln!(
                "  тарифы до отстоя: {e} — излишек остаётся «Затягивание грузовой операции»"
            ),
        }

        let placed: i32 = reserve_assignments.iter().map(|a| a.quantity).sum();
        let used_stations: HashSet<&str> = reserve_assignments
            .iter()
            .map(|a| reserve_nodes[a.r_idx].station_code.as_str())
            .collect();
        let reserve_cost: f64 = reserve_assignments
            .iter()
            .map(|a| a.cost * a.quantity as f64)
            .sum();
        println!(
            "В отстой: {} из {} ваг. излишка → {} станций отстоя, тариф {:.0} руб.; не размещено {} ваг. (нет тарифа / ёмкость исчерпана / дальше потолка дальности)",
            placed,
            total_excess,
            used_stations.len(),
            reserve_cost,
            total_excess - placed,
        );
    } else if total_excess > 0 {
        println!(
            "В отстой: пропущено (резервы не загружены), излишек {} ваг. остаётся «Затягивание»",
            total_excess,
        );
    }

    // -----------------------------------------------------------------------
    // 6б. Этап 3: размещение оставшегося излишка на свободных подъездных путях
    //     крупных станций погрузки (data/load_stations_free_capacity.json).
    //     На вход — остаток ПОСЛЕ отстоя. Тарифы запрашиваются от станций излишка
    //     ко всем станциям погрузки из справочника. Ограничение: на одну станцию
    //     не менее LOADROAD_MIN_BATCH (=5) вагонов. ДМЗИ не расходуется.
    //     Пары дальше MaxReserveEmptyRunDistanceKm (ДВС/ЗАБ — FarEast) не строятся.
    // -----------------------------------------------------------------------
    let mut loadroad_assignments: Vec<solver::LoadRoadAssignment> = Vec::new();
    // Остаток после отстоя: вычитаем размещённое в резервы из остатка основного решения.
    let mut excess_after_reserve = remaining_supply_vec.clone();
    for ra in &reserve_assignments {
        if let Some(rem) = excess_after_reserve.get_mut(ra.s_idx) {
            *rem -= ra.quantity;
        }
    }
    let total_excess_after_reserve: i32 = excess_after_reserve.iter().map(|&r| r.max(0)).sum();
    if !free_loadroads.is_empty() && total_excess_after_reserve > 0 {
        let excess_from: Vec<StationRef> = opt_supply
            .iter()
            .zip(excess_after_reserve.iter())
            .filter(|&(_, &rem)| rem > 0)
            .map(|(s, _)| (s.station_to_code.clone(), s.railway_to.clone()))
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|(code, rw)| StationRef::new(code, rw))
            .collect();
        // Тарифы ко ВСЕМ станциям погрузки из справочника (уникальные код+дорога).
        let loadroad_refs: Vec<StationRef> = free_loadroads
            .iter()
            .map(|l| (l.load_station_code.clone(), l.load_road_name.clone()))
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|(code, rw)| StationRef::new(code, rw))
            .collect();

        match client.fetch_tariffs(&excess_from, &loadroad_refs).await {
            Ok(items) => {
                let loadroad_tariff_map: HashMap<(String, String), TariffNode> = items
                    .into_iter()
                    .map(|t| ((t.station_from_code.clone(), t.station_to_code.clone()), t))
                    .collect();
                println!(
                    "Тарифов до путей погрузки:   {} (станций излишка {}, станций погрузки {})",
                    loadroad_tariff_map.len(),
                    excess_from.len(),
                    loadroad_refs.len(),
                );
                loadroad_assignments = solver::solve_loadroad_assignment(
                    &excess_after_reserve,
                    &opt_supply,
                    &free_loadroads,
                    &loadroad_tariff_map,
                    &business_rules,
                );
            }
            Err(e) => eprintln!(
                "  тарифы до путей погрузки: {e} — остаток остаётся «Затягивание грузовой операции»"
            ),
        }

        let placed: i32 = loadroad_assignments.iter().map(|a| a.quantity).sum();
        let used_stations: HashSet<&str> = loadroad_assignments
            .iter()
            .map(|a| free_loadroads[a.l_idx].load_station_code.as_str())
            .collect();
        let loadroad_cost: f64 = loadroad_assignments
            .iter()
            .map(|a| a.cost * a.quantity as f64)
            .sum();
        println!(
            "На пути погрузки: {} из {} ваг. остатка → {} станций (≥{} ваг./станция), тариф {:.0} руб.; не размещено {} ваг. (нет тарифа / ёмкость / min-batch / дальше потолка дальности)",
            placed,
            total_excess_after_reserve,
            used_stations.len(),
            solver::LOADROAD_MIN_BATCH,
            loadroad_cost,
            total_excess_after_reserve - placed,
        );
    } else if total_excess_after_reserve > 0 {
        println!(
            "На пути погрузки: пропущено (справочник пуст), остаток {} ваг. остаётся «Затягивание»",
            total_excess_after_reserve,
        );
    }

    // -----------------------------------------------------------------------
    // 7. Построение выходных записей + сохранение чекпоинта и отправка в АПИ
    // -----------------------------------------------------------------------
    // Записи: оптимизация (Free / NoNumber) + отстой (этап 2) + пути погрузки (этап 3).
    let mut output_records = solver::build_output_records(
        &solution, &arcs, &opt_supply, &demand_lp, &wash_codes, foreign_washed_roads,
        &washed_empty_codes, &reserve_assignments, &reserve_nodes,
        &loadroad_assignments, &free_loadroads,
    );
    // Самопроверка баланса: вагоны не должны «исчезать» из отчёта — каждый вагон
    // предложения либо назначен, либо получает «Затягивание грузовой операции».
    {
        let (cars_recs, cars_sup) = solver::output_balance(&output_records, &opt_supply);
        if cars_recs == cars_sup {
            println!("Баланс отчёта оптимизации:   OK ({} ваг. в записях = {} ваг. предложения)", cars_recs, cars_sup);
        } else {
            eprintln!(
                "  ВНИМАНИЕ: баланс отчёта нарушен — {} ваг. в записях != {} ваг. предложения \
                 (потеряно {} ваг.). Проверьте build_output_records.",
                cars_recs,
                cars_sup,
                cars_sup - cars_recs,
            );
        }
    }
    // Добавляем вагоны "По факту" (Assigned): ShipmentGoalId из DislocationPreview → тип назначения.
    let assigned_car_numbers: Vec<u64> = assigned_nodes
        .iter()
        .flat_map(|s| s.car_numbers.iter().copied())
        .collect();
    let shipment_goals = match data::dislocations::fetch_shipment_goals_for_car_numbers(
        &assigned_car_numbers,
    ) {
        Ok(m) => m,
        Err(e) => {
            eprintln!(
                "  ShipmentGoalId для Assigned: не загружен ({e}); для всех — «По факту»"
            );
            std::collections::HashMap::new()
        }
    };
    // Assigned-вагоны сохраняют исходные назначения без изменений.
    let assigned_records = solver::build_assigned_output_records(
        &assigned_nodes,
        &tariff_nodes,
        &shipment_goals,
    );

    // Вагоны «В ремонт» (NeedsRepair): выбираем ремонтную станцию с min тарифом,
    // грузополучатель берётся из словаря repairs.json.
    let repair_records = solver::build_repair_output_records(
        &repair_nodes, &repair_tariffs, &repair_stations, &convention_index,
    );

    let n_optim    = output_records.len();
    let n_assigned = assigned_records.len();
    let n_repair   = repair_records.len();
    output_records.extend(assigned_records);
    output_records.extend(repair_records);

    // Количество вагонов (сумма assigned_cars) и записей для Excel.
    let cars_excel: i32 = output_records.iter().map(|r| r.assigned_cars).sum();

    let api_records = solver::output_records_for_api(&output_records);
    let n_api       = api_records.len();
    let cars_api: i32 = api_records.iter().map(|r| r.assigned_cars).sum();

    // Вагоны дислокации (supply_period == 10): исключены из POST АПИ.
    let (n_skip_p10, cars_skip_p10) = output_records
        .iter()
        .filter(|r| r.supply_period == 10)
        .fold((0usize, 0i32), |(recs, cars), r| (recs + 1, cars + r.assigned_cars));

    println!(
        "Записей в отчёте (Excel):    {} ({} оптим. + {} по факту + {} в ремонт) / {} вагонов",
        output_records.len(), n_optim, n_assigned, n_repair, cars_excel,
    );
    println!(
        "  → в POST АПИ (период 1):   {} записей / {} вагонов",
        n_api, cars_api,
    );
    if n_skip_p10 > 0 {
        println!(
            "  → исключено (предл. 10, дислокация): {} записей / {} вагонов",
            n_skip_p10, cars_skip_p10,
        );
    }
    // Контрольная сумма: если API + p10 != Excel — есть иная причина.
    if cars_api + cars_skip_p10 != cars_excel {
        eprintln!(
            "  [!] Нераскрытая разница: Excel {} вагонов ≠ API {} + p10 {} = {} вагонов",
            cars_excel, cars_api, cars_skip_p10, cars_api + cars_skip_p10,
        );
    }

    let demand_checkpoint = demand_lp.clone();
    let report_tag = day1_file_tag();
    let checkpoint = debug::save_checkpoint(
        &demand_checkpoint,
        &supply_nodes,
        Some(&output_records),
        report_tag,
    )?;
    println!("Чекпоинт сохранён:           {}", checkpoint.display());

    // match client.send_assignments(&api_records).await {
    //     Ok(())   => println!("Назначения отправлены в АПИ: OK"),
    //     Err(e)   => eprintln!("Ошибка отправки в АПИ:       {e}"),
    // }

    // -----------------------------------------------------------------------
    // 8. Вывод результатов в терминал
    // -----------------------------------------------------------------------
    println!();
    println!("======= РЕЗУЛЬТАТЫ ОПТИМИЗАЦИИ =======");
    println!("Статус решателя:      {}", optim_result.status);
    println!("Назначено вагонов:    {:.0}", optim_result.assigned_cars);
    if optim_result.excess_supply > 1e-4 {
        println!("Избыток предложения:  {:.0} ваг. (dummy-спрос)", optim_result.excess_supply);
        println!(
            "  остаток по периодам предложения: p1={} p10={} прочие={}",
            remaining_supply_p1, remaining_supply_p10, remaining_supply_other
        );
        let reserve_placed: i32 = reserve_assignments.iter().map(|a| a.quantity).sum();
        if reserve_placed > 0 {
            println!("  из них в отстой (этап 2): {} ваг.", reserve_placed);
        }
        let loadroad_placed: i32 = loadroad_assignments.iter().map(|a| a.quantity).sum();
        if loadroad_placed > 0 {
            println!("  из них на пути погрузки (этап 3): {} ваг.", loadroad_placed);
        }
    }
    if optim_result.penalty_cars > 1e-4 {
        println!("Неудовл. спрос:       {:.0} ваг. (dummy-предложение)", optim_result.penalty_cars);
    }
    println!(
        "Суммарная стоимость:  {:.0} руб.",
        optim_result.total_cost
    );
    println!("======================================");
    println!();

    // -----------------------------------------------------------------------
    // 9. Сохранение результатов в tmp/result_*.json
    // -----------------------------------------------------------------------
    let report = solver::build_report(
        &optim_result,
        &solution,
        &arcs,
        &opt_supply,
        &demand_lp,
    );

    let result_path = solver::save_result(&report, report_tag)?;
    println!("Результат сохранён:          {}", result_path.display());

    Ok(())
}

/// `INCLUDE_PERIOD10` по умолчанию on: в пул входят вагоны периода 1 и дислокация периода 10.
/// `off` / `0` / `false` / `no` — только 1-е сутки (`run.sh --day1`).
fn include_period10_from_env() -> bool {
    std::env::var("INCLUDE_PERIOD10")
        .map(|v| {
            let v = v.trim().to_lowercase();
            !matches!(v.as_str(), "off" | "0" | "false" | "no" | "none")
        })
        .unwrap_or(true)
}

/// Пометка в именах `tmp/result_*.json` и `tmp/checkpoint_*.xlsx` для прогона `--day1`.
fn day1_file_tag() -> Option<&'static str> {
    (!include_period10_from_env()).then_some("day1")
}

/// Режим правила 3. `GU12_MODE=strong` — потолок на каждый период (`run.sh --strong-gu12`).
/// Пусто, `relaxed` и любое неизвестное значение — горизонт 1–15 суток (`--relaxed-gu12`, по умолчанию).
fn gu12_mode_from_env() -> data::Gu12Mode {
    match std::env::var("GU12_MODE") {
        Ok(v) => match v.trim().to_lowercase().as_str() {
            "strong" => data::Gu12Mode::Strong,
            "relaxed" | "" => data::Gu12Mode::Relaxed,
            other => {
                eprintln!(
                    "  [!] GU12_MODE={other:?} не известен — берётся ослабленный режим ГУ-12 (горизонт 1–15 суток)"
                );
                data::Gu12Mode::Relaxed
            }
        },
        Err(_) => data::Gu12Mode::Relaxed,
    }
}

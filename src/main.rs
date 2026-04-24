mod config;
mod data;
mod debug;
mod node;
mod solver;

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use config::Config;
use data::{ApiClient, StationRef};
use node::{CarKind, DemandNode, DemandPurpose, RepairStatus, TariffNode};

#[tokio::main]
async fn main() -> Result<()> {
    // -----------------------------------------------------------------------
    // 1. Конфигурация и API-клиент
    // -----------------------------------------------------------------------
    let cfg    = Config::from_env()?;
    let client = ApiClient::new(&cfg.api_base_url, &cfg.api_token)?;

    // -----------------------------------------------------------------------
    // 2. Получение данных спроса и предложения
    // -----------------------------------------------------------------------
    let demand_nodes = client.fetch_demand_nodes().await?;
    let demand_total_cars: i32 = demand_nodes.iter()
                .map(|d| d.car_count)
                .sum();
    println!("Получено узлов спроса (погрузка): {} или {} вагонов", demand_nodes.len(), demand_total_cars);

    let mut supply_nodes = client.fetch_supply_nodes().await?;
    let supply1_total_cars: i32 = supply_nodes.iter()
                .map(|s| s.car_count)
                .sum();
    println!("Получено узлов предложения 1 сут.:  {} или {} вагонов", supply_nodes.len(), supply1_total_cars);

    match data::dislocations::fetch_dislocation_supply_nodes() {
        Ok(extra) if !extra.is_empty() => {
            let extra_total_cars: i32 = extra.iter()
                .map(|e| e.car_count)
                .sum();
            println!(
                "  узлов дислокации (2-10 сут., период 10): {} или {} вагонов",
                extra.len(),
                extra_total_cars
            );
            supply_nodes.extend(extra);
        }
        Ok(_) => {}
        Err(e) => eprintln!(
            "  дислокация 2-10 сут.: не загружена ({}), продолжаем только АПИ",
            e
        ),
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
    let no_cleaning_roads = match data::load_no_cleaning_roads("data/references.json") {
        Ok(r) => {
            println!("Дороги без промывки (NoCleaningRoads): {}", r.len());
            r
        }
        Err(e) => {
            eprintln!("  NoCleaningRoads из references.json: не загружены ({e})");
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

    // Все вагоны с «грязным» ETSNG (без учёта NoCleaningRoads).
    let n_supply_wash_raw = opt_supply
        .iter()
        .filter(|s| data::wash::supply_matches_wash_product_list(s, &wash_codes))
        .map(|s| s.car_count)
        .sum::<i32>();
    // Из них освобождены от промывки по дороге образования (NoCleaningRoads).
    let n_supply_wash_exempt = opt_supply
        .iter()
        .filter(|s| {
            data::wash::supply_matches_wash_product_list(s, &wash_codes)
                && no_cleaning_roads.contains(s.railway_to.trim())
        })
        .map(|s| s.car_count)
        .sum::<i32>();
    // Итого «грязных», требующих промывки.
    let n_supply_wash_list = n_supply_wash_raw - n_supply_wash_exempt;
    let n_supply_wash_skip = opt_supply
        .iter()
        .filter(|s| {
            data::wash::supply_needs_wash(s, &wash_codes, &no_cleaning_roads)
                && data::wash::load_demand_covers_same_etsng(s, &demand_nodes)
        })
        .map(|s| s.car_count)
        .sum::<i32>();
    println!(
        "  предложений с ЕТСНГ из списка промывки: {} вагонов (освобождены по NoCleaningRoads: {}; из них погрузка того же ЕТСНГ — промывка не обязательна: {} вагонов)",
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
    // 3б. Тарифы до станций промывки + надбавки (промывка + порожний пробег до погрузки).
    //     В LP используется только суммарная стоимость дуги «до промывки».
    //     FrETSNGCode: груженый — текущий груз, порожний — PrevFrETSNG (доминирующий в группе).
    // -----------------------------------------------------------------------
    let wash_station_refs = data::wash::wash_station_refs(&wash_stations);
    let mut wash_tariff_map: HashMap<(String, String), TariffNode> = HashMap::new();
    if !wash_station_refs.is_empty() {
        let wash_from: Vec<StationRef> = opt_supply
            .iter()
            .filter(|s| data::wash::supply_needs_wash(s, &wash_codes, &no_cleaning_roads))
            .map(|s| (s.station_to_code.clone(), s.railway_to.clone()))
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|(code, rw)| StationRef::new(code, rw))
            .collect();

        if !wash_from.is_empty() {
            match client.fetch_tariffs(&wash_from, &wash_station_refs).await {
                Ok(items) => {
                    for mut t in items {
                        t.cost += solver::WASH_PATH_SURCHARGE_RUB;
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
                solver::WASH_PROCEDURE_AVG_COST_RUB as i64,
                solver::EMPTY_RUN_AFTER_WASH_TO_LOAD_AVG_COST_RUB as i64,
                solver::WASH_PATH_SURCHARGE_RUB as i64,
                wash_tariff_map.len(),
            );
        }
    } else if !wash_codes.is_empty() && wash_stations.is_empty() {
        println!("Тарифы до промывки:         не запрошены (нет станций промывки)");
    }

    // -----------------------------------------------------------------------
    // 4. Построение дуг транспортной задачи
    // -----------------------------------------------------------------------
    let (arcs, arc_stats) = solver::build_task_arcs(
        &opt_supply,
        &demand_lp,
        &tariff_nodes,
        &wash_codes,
        &no_cleaning_roads,
        &wash_tariff_map,
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
        "  грязный вагон → чужой ЕТСНГ:       {} ({:.1}%)",
        arc_stats.dirty_etsng_mismatch,
        100.0 * arc_stats.dirty_etsng_mismatch as f64 / total.max(1) as f64,
    );
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

    // -----------------------------------------------------------------------
    // 5. Анализ баланса и начальное жадное решение
    // -----------------------------------------------------------------------
    solver::print_balance(&opt_supply, &demand_lp);

    let greedy_result = solver::greedy_initial_solution(&arcs, &opt_supply, &demand_lp);
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
        if !mip_outcome.is_globally_optimal() && mip_undist > greedy_undist {
            println!(
                "  ВНИМАНИЕ: MIP хуже greedy по числу нераспределённых вагонов. Возможные причины:\n           (а) санитизация warm-start сняла вагоны с пар (0 < sum < MIN_BATCH),\n           (б) PENALTY_COST ниже стоимости единственно допустимых плеч,\n           (в) HiGHS остановился по rel_gap до полного перераспределения."
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
        // должно совпадать с accept-критерием ALNS — иначе ALNS сразу же
        // может «откатить» seed обратно:
        //   1) меньше нераспределённых вагонов (unmet + excess);
        //   2) при равенстве — меньше unmet (Load-покрытие важнее excess);
        //   3) при полной ничье — ниже real_cost.
        let mip_as_greedy = solver::arc_vals_to_greedy_result(
            &mip_outcome.arc_vals, &arcs, &opt_supply, &demand_lp,
        );
        let greedy_undist = greedy_result.unmet_demand + greedy_result.excess_supply;
        let mip_undist    = mip_as_greedy.unmet_demand + mip_as_greedy.excess_supply;

        // Кортежи для лексикографического сравнения.
        // Для real_cost используем i64 (округляем до рубля) — для lex ок.
        let greedy_key = (greedy_undist, greedy_result.unmet_demand, greedy_result.total_cost as i64);
        let mip_key    = (mip_undist,    mip_as_greedy.unmet_demand, mip_as_greedy.total_cost as i64);

        let alns_seed = if mip_key < greedy_key { &mip_as_greedy } else { &greedy_result };
        let seed_name = if mip_key < greedy_key { "MIP" } else { "greedy" };

        println!("--- SEED ДЛЯ ALNS ---");
        println!(
            "  greedy : undist {:>4} (unmet {:>3} + excess {:>3}), assigned {:>4}, real_cost {:>12.2} руб.",
            greedy_undist, greedy_result.unmet_demand, greedy_result.excess_supply,
            greedy_result.assigned_cars, greedy_result.total_cost,
        );
        println!(
            "  MIP    : undist {:>4} (unmet {:>3} + excess {:>3}), assigned {:>4}, real_cost {:>12.2} руб.",
            mip_undist, mip_as_greedy.unmet_demand, mip_as_greedy.excess_supply,
            mip_as_greedy.assigned_cars, mip_as_greedy.total_cost,
        );
        println!(
            "  выбран : {} (критерий: min(undist), затем min(unmet), затем min(real_cost))",
            seed_name,
        );
        println!("---------------------");

        let alns_config = solver::AlnsConfig::default();
        let alns_result = solver::run_alns(
            alns_seed, &arcs, &opt_supply, &demand_lp, &alns_config,
        );
        let optim_result = alns_result.to_optim_result(&demand_lp);
        let solution     = alns_result.arc_vals.clone();
        let rem          = alns_result.best_state.remaining_supply.clone();
        (optim_result, solution, rem)
    };

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
    // 7. Построение выходных записей + сохранение чекпоинта и отправка в АПИ
    // -----------------------------------------------------------------------
    // Записи по оптимизированным назначениям (Free / NoNumber).
    let mut output_records = solver::build_output_records(
        &solution, &arcs, &opt_supply, &demand_lp, &wash_codes, &no_cleaning_roads,
    );
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
        &repair_nodes, &repair_tariffs, &repair_stations,
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
    let checkpoint =
        debug::save_checkpoint(&demand_checkpoint, &supply_nodes, Some(&output_records))?;
    println!("Чекпоинт сохранён:           {}", checkpoint.display());

    match client.send_assignments(&api_records).await {
        Ok(())   => println!("Назначения отправлены в АПИ: OK"),
        Err(e)   => eprintln!("Ошибка отправки в АПИ:       {e}"),
    }

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

    let result_path = solver::save_result(&report)?;
    println!("Результат сохранён:          {}", result_path.display());

    Ok(())
}

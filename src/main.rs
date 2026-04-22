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

    let n_supply_wash_list = opt_supply
        .iter()
        .filter(|s| data::wash::supply_matches_wash_product_list(s, &wash_codes))
        .map(|s| s.car_count)
        .sum::<i32>();
    let n_supply_wash_skip = opt_supply
        .iter()
        .filter(|s| {
            data::wash::supply_matches_wash_product_list(s, &wash_codes)
                && data::wash::load_demand_covers_same_etsng(s, &demand_nodes)
        })
        .map(|s| s.car_count)
        .sum::<i32>();
    println!(
        "  предложений с ЕТСНГ из списка промывки: {} вагонов (из них погрузка того же ЕТСНГ на станции — промывка не обязательна: {} вагонов)",
        n_supply_wash_list, n_supply_wash_skip
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
            .filter(|s| data::wash::supply_matches_wash_product_list(s, &wash_codes))
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

    // // -----------------------------------------------------------------------
    // // 4. Построение дуг транспортной задачи
    // // -----------------------------------------------------------------------
    // let (arcs, arc_stats) = solver::build_task_arcs(
    //     &opt_supply,
    //     &demand_lp,
    //     &tariff_nodes,
    //     &wash_codes,
    //     &wash_tariff_map,
    // );

    // let total = arc_stats.total_pairs;
    // println!("Всего пар supply×demand:     {}", total);
    // println!(
    //     "  без тарифа:                {} ({:.1}%)",
    //     arc_stats.no_tariff,
    //     100.0 * arc_stats.no_tariff as f64 / total.max(1) as f64,
    // );
    // println!(
    //     "  нарушение срока (жёстко):  {} ({:.1}%)",
    //     arc_stats.bad_period,
    //     100.0 * arc_stats.bad_period as f64 / total.max(1) as f64,
    // );
    // println!(
    //     "  дуг со штрафом за срок:    {} ({:.1}%)",
    //     arc_stats.arcs_period_penalized,
    //     100.0 * arc_stats.arcs_period_penalized as f64 / total.max(1) as f64,
    // );
    // println!(
    //     "  несовм. тип вагона:        {} ({:.1}%)",
    //     arc_stats.bad_type,
    //     100.0 * arc_stats.bad_type as f64 / total.max(1) as f64,
    // );
    // println!(
    //     "Допустимых дуг в LP:         {} ({:.1}%)",
    //     arc_stats.feasible,
    //     100.0 * arc_stats.feasible as f64 / total.max(1) as f64,
    // );

    // // -----------------------------------------------------------------------
    // // 5. Анализ баланса и начальное жадное решение
    // // -----------------------------------------------------------------------
    // solver::print_balance(&opt_supply, &demand_lp);

    // let greedy_result = solver::greedy_initial_solution(&arcs, &opt_supply, &demand_lp);
    // solver::print_greedy_result(&greedy_result, &opt_supply, &demand_lp);

    // // -----------------------------------------------------------------------
    // // 6. ALNS-оптимизация (Adaptive Large Neighbourhood Search)
    // // -----------------------------------------------------------------------
    // let alns_config = solver::AlnsConfig::default();
    // let alns_result = solver::run_alns(
    //     &greedy_result, &arcs, &opt_supply, &demand_lp, &alns_config,
    // );
    // let optim_result  = alns_result.to_optim_result(&demand_lp);
    // let solution      = alns_result.arc_vals;
    // let mut remaining_supply_p1 = 0_i32;
    // let mut remaining_supply_p10 = 0_i32;
    // for (s, &rem) in opt_supply
    //     .iter()
    //     .zip(alns_result.best_state.remaining_supply.iter())
    // {
    //     if rem <= 0 {
    //         continue;
    //     }
    //     match s.supply_period {
    //         1 => remaining_supply_p1 += rem,
    //         10 => remaining_supply_p10 += rem,
    //         _ => {}
    //     }
    // }
    // let remaining_supply_other = (optim_result.excess_supply as i32
    //     - remaining_supply_p1
    //     - remaining_supply_p10)
    //     .max(0);

    // // -----------------------------------------------------------------------
    // // 7. Построение выходных записей + сохранение чекпоинта и отправка в АПИ
    // // -----------------------------------------------------------------------
    // // Записи по оптимизированным назначениям (Free / NoNumber).
    // let mut output_records = solver::build_output_records(
    //     &solution, &arcs, &opt_supply, &demand_lp, &wash_codes,
    // );
    // // Добавляем вагоны "По факту" (Assigned): ShipmentGoalId из DislocationPreview → тип назначения.
    // let assigned_car_numbers: Vec<u64> = assigned_nodes
    //     .iter()
    //     .flat_map(|s| s.car_numbers.iter().copied())
    //     .collect();
    // let shipment_goals = match data::dislocations::fetch_shipment_goals_for_car_numbers(
    //     &assigned_car_numbers,
    // ) {
    //     Ok(m) => m,
    //     Err(e) => {
    //         eprintln!(
    //             "  ShipmentGoalId для Assigned: не загружен ({e}); для всех — «По факту»"
    //         );
    //         std::collections::HashMap::new()
    //     }
    // };
    // // Assigned-вагоны сохраняют исходные назначения без изменений.
    // let assigned_records = solver::build_assigned_output_records(
    //     &assigned_nodes,
    //     &tariff_nodes,
    //     &shipment_goals,
    // );

    // // Вагоны «В ремонт» (NeedsRepair): выбираем ремонтную станцию с min тарифом,
    // // грузополучатель берётся из словаря repairs.json.
    // let repair_records = solver::build_repair_output_records(
    //     &repair_nodes, &repair_tariffs, &repair_stations,
    // );

    // let n_optim    = output_records.len();
    // let n_assigned = assigned_records.len();
    // let n_repair   = repair_records.len();
    // output_records.extend(assigned_records);
    // output_records.extend(repair_records);

    // let api_records = solver::output_records_for_api(&output_records);
    // let n_api       = api_records.len();
    // let n_skip_10   = output_records.len() - n_api;
    // println!(
    //     "Записей в отчёте (Excel):    {} ({} оптим. + {} по факту + {} в ремонт)",
    //     output_records.len(),
    //     n_optim,
    //     n_assigned,
    //     n_repair,
    // );
    // if n_skip_10 > 0 {
    //     println!(
    //         "  в POST АПИ (только 1 сут.): {} (без периода предл. 10: {})",
    //         n_api,
    //         n_skip_10,
    //     );
    // } else {
    //     println!("Записей в POST АПИ:          {}", n_api);
    // }

    // let demand_checkpoint = demand_lp.clone();
    // let checkpoint =
    //     debug::save_checkpoint(&demand_checkpoint, &supply_nodes, Some(&output_records))?;
    // println!("Чекпоинт сохранён:           {}", checkpoint.display());

    // // match client.send_assignments(&api_records).await {
    // //     Ok(())   => println!("Назначения отправлены в АПИ: OK"),
    // //     Err(e)   => eprintln!("Ошибка отправки в АПИ:       {e}"),
    // // }

    // // -----------------------------------------------------------------------
    // // 8. Вывод результатов в терминал
    // // -----------------------------------------------------------------------
    // println!();
    // println!("======= РЕЗУЛЬТАТЫ ОПТИМИЗАЦИИ =======");
    // println!("Статус решателя:      {}", optim_result.status);
    // println!("Назначено вагонов:    {:.0}", optim_result.assigned_cars);
    // if optim_result.excess_supply > 1e-4 {
    //     println!("Избыток предложения:  {:.0} ваг. (dummy-спрос)", optim_result.excess_supply);
    //     println!(
    //         "  остаток по периодам предложения: p1={} p10={} прочие={}",
    //         remaining_supply_p1, remaining_supply_p10, remaining_supply_other
    //     );
    // }
    // if optim_result.penalty_cars > 1e-4 {
    //     println!("Неудовл. спрос:       {:.0} ваг. (dummy-предложение)", optim_result.penalty_cars);
    // }
    // println!(
    //     "Суммарная стоимость:  {:.0} руб.",
    //     optim_result.total_cost
    // );
    // println!("======================================");
    // println!();

    // // -----------------------------------------------------------------------
    // // 9. Сохранение результатов в tmp/result_*.json
    // // -----------------------------------------------------------------------
    // let report = solver::build_report(
    //     &optim_result,
    //     &solution,
    //     &arcs,
    //     &opt_supply,
    //     &demand_lp,
    // );

    // let result_path = solver::save_result(&report)?;
    // println!("Результат сохранён:          {}", result_path.display());

    Ok(())
}

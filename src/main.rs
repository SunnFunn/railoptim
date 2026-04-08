mod config;
mod data;
mod debug;
mod node;
mod solver;

use std::collections::HashSet;

use anyhow::Result;
use config::Config;
use data::{ApiClient, StationRef};
use node::CarKind;

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
    println!("Получено узлов спроса:       {}", demand_nodes.len());

    let mut supply_nodes = client.fetch_supply_nodes().await?;
    match data::dislocations::fetch_dislocation_supply_nodes() {
        Ok(extra) if !extra.is_empty() => {
            println!(
                "  узлов дислокации (2-10 сут., период 10): {}",
                extra.len()
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
    println!("Получено узлов предложения:  {}", supply_nodes.len());

    // Разделяем: Assigned — уже назначены и не участвуют в оптимизации.
    let (assigned_nodes, opt_supply): (Vec<_>, Vec<_>) = supply_nodes
        .iter()
        .cloned()
        .partition(|s| s.kind == CarKind::Assigned);
    println!("  свободных для назначения:  {}", opt_supply.len());
    println!("  по факту (Assigned):       {}", assigned_nodes.len());

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
    // 4. Построение дуг транспортной задачи
    // -----------------------------------------------------------------------
    let (arcs, arc_stats) = solver::build_task_arcs(&opt_supply, &demand_nodes, &tariff_nodes);

    let total = arc_stats.total_pairs;
    println!("Всего пар supply×demand:     {}", total);
    println!(
        "  без тарифа:                {} ({:.1}%)",
        arc_stats.no_tariff,
        100.0 * arc_stats.no_tariff as f64 / total.max(1) as f64,
    );
    println!(
        "  нарушение срока:           {} ({:.1}%)",
        arc_stats.bad_period,
        100.0 * arc_stats.bad_period as f64 / total.max(1) as f64,
    );
    println!(
        "  несовм. тип вагона:        {} ({:.1}%)",
        arc_stats.bad_type,
        100.0 * arc_stats.bad_type as f64 / total.max(1) as f64,
    );
    println!(
        "Допустимых дуг в LP:         {} ({:.1}%)",
        arc_stats.feasible,
        100.0 * arc_stats.feasible as f64 / total.max(1) as f64,
    );

    // -----------------------------------------------------------------------
    // 5. Анализ баланса и начальное жадное решение
    // -----------------------------------------------------------------------
    solver::print_balance(&opt_supply, &demand_nodes);

    let greedy_result = solver::greedy_initial_solution(&arcs, &opt_supply, &demand_nodes);
    solver::print_greedy_result(&greedy_result, &opt_supply, &demand_nodes);

    // -----------------------------------------------------------------------
    // 6. ALNS-оптимизация (Adaptive Large Neighbourhood Search)
    // -----------------------------------------------------------------------
    let alns_config = solver::AlnsConfig::default();
    let alns_result = solver::run_alns(
        &greedy_result, &arcs, &opt_supply, &demand_nodes, &alns_config,
    );
    let optim_result  = alns_result.to_optim_result();
    let solution      = alns_result.arc_vals;

    // -----------------------------------------------------------------------
    // 7. Построение выходных записей + сохранение чекпоинта и отправка в АПИ
    // -----------------------------------------------------------------------
    // Записи по оптимизированным назначениям (Free / NoNumber).
    let mut output_records = solver::build_output_records(
        &solution, &arcs, &opt_supply, &demand_nodes,
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
    let assigned_records = solver::build_assigned_output_records(
        &assigned_nodes,
        &tariff_nodes,
        &shipment_goals,
    );
    let n_optim    = output_records.len();
    let n_assigned = assigned_records.len();
    output_records.extend(assigned_records);

    let api_records = solver::output_records_for_api(&output_records);
    let n_api       = api_records.len();
    let n_skip_10   = output_records.len() - n_api;
    println!(
        "Записей в отчёте (Excel):    {} ({} оптим. + {} по факту)",
        output_records.len(),
        n_optim,
        n_assigned,
    );
    if n_skip_10 > 0 {
        println!(
            "  в POST АПИ (только 1 сут.): {} (без периода предл. 10: {})",
            n_api,
            n_skip_10,
        );
    } else {
        println!("Записей в POST АПИ:          {}", n_api);
    }

    let checkpoint = debug::save_checkpoint(&demand_nodes, &supply_nodes, Some(&output_records))?;
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
        &demand_nodes,
    );

    let result_path = solver::save_result(&report)?;
    println!("Результат сохранён:          {}", result_path.display());

    Ok(())
}

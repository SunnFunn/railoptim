mod config;
mod data;
mod debug;
mod node;
mod solver;

use std::collections::HashSet;

use anyhow::Result;
use config::Config;
use data::{ApiClient, StationRef};

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

    let supply_nodes = client.fetch_supply_nodes().await?;
    println!("Получено узлов предложения:  {}", supply_nodes.len());

    let checkpoint = debug::save_checkpoint(&demand_nodes, &supply_nodes)?;
    println!("Чекпоинт сохранён:           {}", checkpoint.display());

    // -----------------------------------------------------------------------
    // 3. Получение тарифов
    //    stations_from — уникальные станции образования порожних (station_to)
    //    stations_to   — уникальные станции погрузки (station_code)
    // -----------------------------------------------------------------------
    let stations_from: Vec<StationRef> = supply_nodes
        .iter()
        .map(|s| (s.station_to_code.clone(), s.railway_to.clone()))
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|(code, rw)| StationRef::new(code, rw))
        .collect();

    let stations_to: Vec<StationRef> = demand_nodes
        .iter()
        .map(|d| (d.station_code.clone(), d.railway_name.clone()))
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|(code, rw)| StationRef::new(code, rw))
        .collect();

    let tariff_nodes = client.fetch_tariffs(&stations_from, &stations_to).await?;
    println!("Получено тарифов:            {}", tariff_nodes.len());

    // -----------------------------------------------------------------------
    // 4. Построение дуг транспортной задачи
    // -----------------------------------------------------------------------
    let (arcs, arc_stats) = solver::build_task_arcs(&supply_nodes, &demand_nodes, &tariff_nodes);

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
    // 5. Анализ баланса и оптимизация
    // -----------------------------------------------------------------------
    solver::print_balance(&supply_nodes, &demand_nodes);

    // println!("{:?}", &supply_nodes[..5]);
    // println!("{:?}", &demand_nodes[..5]);
    // println!("{:?}", &arcs[..5]);

    let (optim_result, solution) =
        solver::solve(&arcs, &supply_nodes, &demand_nodes);

    // -----------------------------------------------------------------------
    // 6. Вывод результатов в терминал
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
    // 7. Сохранение результатов в tmp/result_*.json
    // -----------------------------------------------------------------------
    let report = solver::build_report(
        &optim_result,
        &solution,
        &arcs,
        &supply_nodes,
        &demand_nodes,
    );

    let result_path = solver::save_result(&report)?;
    println!("Результат сохранён:          {}", result_path.display());

    Ok(())
}

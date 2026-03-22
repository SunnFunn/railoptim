use highs::{ColProblem, Sense};
use ndarray::Array2;
use rust_xlsxwriter::*;

use crate::node::Node;

// 1. Функция анализа баланса (вызывать ДО solve)
pub fn print_balance(task_array: &Array2<Node>) {
    let s_size = task_array.nrows();
    let d_size = task_array.ncols();

    // Считаем суммы по краям матрицы
    let total_supply: i32 = (0..s_size).map(|i| task_array[(i, 0)].s_qty).sum();
    let total_demand: i32 = (0..d_size).map(|j| task_array[(0, j)].d_qty).sum();
    let diff = total_supply - total_demand;

    println!("--- АНАЛИЗ РЕСУРСОВ ---");
    println!("Total Supply: {}", total_supply);
    println!("Total Demand: {}", total_demand);
    if diff >= 0 {
        println!("Статус: ПРОФИЦИТ (+{} ваг.)", diff);
    } else {
        println!("⚠️ Статус: ДЕФИЦИТ ({} ваг. поедут по штрафам)", diff.abs());
    }
    println!("-----------------------");
}


// 2. Структура для хранения результатов (чтобы удобно передавать в main)
#[derive(Debug, Clone)]
pub struct OptimResult {
    pub total_real_cost: f64,
    pub normal_qty: f64,
    pub penalty_qty: f64,
    pub status: String,
}


// 2. Основная функция оптимизации
pub fn solve(task_array2d: &Array2<Node>) -> (OptimResult, Vec<f64>) {
    let s_size = task_array2d.nrows();
    let d_size = task_array2d.ncols();
    let mut model = ColProblem::default();
    let mut rows = Vec::with_capacity(s_size + d_size);

    // Добавление строк и колонок (код из прошлых шагов)
    for i in 0..s_size {
        rows.push(model.add_row(0.0..task_array2d[(i, 0)].s_qty as f64));
    }
    for j in 0..d_size {
        rows.push(model.add_row(task_array2d[(0, j)].d_qty as f64..));
    }
    for i in 0..s_size {
        let r_s = rows[i];
        for j in 0..d_size {
            model.add_column(task_array2d[(i, j)].node_cost as f64, 0.0.., [(r_s, 1.0), (rows[s_size + j], 1.0)]);
        }
    }

    let mut optimizer = model.optimise(Sense::Minimise);
    optimizer.set_option("solver", "simplex");
    optimizer.set_option("presolve", "on");
    optimizer.set_option("parallel", "on");
    optimizer.set_option("threads", 8);

    let solved = optimizer.solve();
    let solution = solved.get_solution();
    
    // Вызываем аналитику сразу или возвращаем данные
    let stats = analyze_results(task_array2d, solution.columns());
    
    (OptimResult {
        total_real_cost: stats.0,
        normal_qty: stats.1,
        penalty_qty: stats.2,
        status: format!("{:?}", solved.status()),
    }, solution.columns().to_vec())
}


// 3. Закрытая (private) функция аналитики
fn analyze_results(task_array: &Array2<Node>, col_solution: &[f64]) -> (f64, f64, f64) {
    let mut real_cost = 0.0;
    let mut n_qty = 0.0;
    let mut p_qty = 0.0;
    const THRESHOLD: f64 = 500_000.0;

    for (idx, &qty) in col_solution.iter().enumerate() {
        if qty > 0.0001 {
            let i = idx / task_array.ncols();
            let j = idx % task_array.ncols();
            let cost = task_array[(i, j)].node_cost as f64;

            if cost < THRESHOLD {
                real_cost += qty * cost;
                n_qty += qty;
            } else {
                p_qty += qty;
            }
        }
    }
    (real_cost, n_qty, p_qty)
}


pub fn save_plan_to_excel(
    task_array: &ndarray::Array2<crate::node::Node>, 
    col_solution: &[f64], 
    names: &std::collections::HashMap<usize, (String, String, String)> // Добавлен 3-й элемент
) -> Result<(), XlsxError> {
    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    
    let header_format = Format::new().set_bold().set_border(FormatBorder::Thin);
    let num_format = Format::new().set_num_format("0");

    // Новые заголовки
    let headers = [
        "Дорога отпр.", 
        "Ст. отпр.", 
        "Пер. образования", 
        "Дорога погр.", 
        "Ст. погр.", 
        "Пер. погрузки", 
        "Вагоны"
    ];
    
    for (col, text) in headers.iter().enumerate() {
        worksheet.write_with_format(0, col as u16, *text, &header_format)?;
    }

    let d_size = task_array.ncols();
    let mut row_idx = 1;

    for (idx, &qty) in col_solution.iter().enumerate() {
        if qty > 0.1 {
            let i = idx / d_size;
            let j = idx % d_size;
            let node = &task_array[(i, j)];

            // Извлекаем кортежи из 3-х элементов
            let (s_name, s_rail, s_period) = names.get(&node.s_node_id).cloned().unwrap_or_default();
            let (d_name, d_rail, d_period) = names.get(&node.d_node_id).cloned().unwrap_or_default();

            // Записываем данные по новым колонкам
            worksheet.write(row_idx, 0, s_rail)?;
            worksheet.write(row_idx, 1, s_name)?;
            worksheet.write(row_idx, 2, s_period)?; // Период образования
            
            worksheet.write(row_idx, 3, d_rail)?;
            worksheet.write(row_idx, 4, d_name)?;
            worksheet.write(row_idx, 5, d_period)?; // Период погрузки
            
            worksheet.write_with_format(row_idx, 6, qty, &num_format)?;
            
            row_idx += 1;
        }
    }

    // Автофильтр на 7 колонок (0-6)
    worksheet.autofilter(0, 0, row_idx - 1, 6)?;
    
    // Настройка ширины
    for col in 0..6 {
        worksheet.set_column_width(col as u16, 22)?;
    }
    worksheet.set_column_width(6, 10)?;

    workbook.save("plan.xlsx")?;
    println!("\n✅ План сохранен в plan.xlsx. Периоды добавлены.");
    Ok(())
}


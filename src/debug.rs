//! Вспомогательные утилиты для отладки и анализа данных.
//! Не используются в production-пути выполнения.

use std::path::PathBuf;

use chrono::Local;
use rust_xlsxwriter::{Format, FormatBorder, Workbook, XlsxError};

use crate::node::DemandNode;

/// Сохраняет данные прогона в файл-чекпоинт `tmp/checkpoint_YYYY-MM-DD_HH-MM-SS.xlsx`.
///
/// Каждый тип данных размещается на отдельном листе:
/// - `DemandNodes` — узлы спроса (реализовано)
/// - `SupplyNodes` — узлы образования порожних (будущий лист)
/// - `Tariffs`     — тарифные данные (будущий лист)
///
/// Папка `tmp/` создаётся автоматически. Поля `Vec<String>` выводятся через ` | `.
pub fn save_checkpoint(nodes: &[DemandNode]) -> Result<PathBuf, XlsxError> {
    let tmp_dir = PathBuf::from("tmp");
    std::fs::create_dir_all(&tmp_dir)?;

    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
    let path = tmp_dir.join(format!("checkpoint_{timestamp}.xlsx"));

    let mut workbook = Workbook::new();
    let ws = workbook.add_worksheet();
    ws.set_name("DemandNodes")?;

    // Форматы
    let hdr = Format::new()
        .set_bold()
        .set_border(FormatBorder::Thin)
        .set_background_color(0x_D9_E1_F2);
    let cell = Format::new().set_border(FormatBorder::Thin);
    let num = Format::new()
        .set_border(FormatBorder::Thin)
        .set_num_format("0");

    // Заголовки и ширины столбцов
    let headers: &[(&str, f64)] = &[
        ("ID",               6.0),
        ("Период",           8.0),
        // Погрузка
        ("Ст. погрузки",    22.0),
        ("Код ст. погр.",   14.0),
        ("Дорога погр.",    18.0),
        ("Код дороги погр.",14.0),
        ("Отд. дороги погр.",18.0),
        // Назначение
        ("Ст. назначения",  22.0),
        ("Код ст. назн.",   14.0),
        ("Дорога назн.",    18.0),
        ("Код дороги назн.",14.0),
        ("Отд. дороги назн.",18.0),
        // Отправитель
        ("Грузоотправитель",22.0),
        ("ОКПО отпр.",      14.0),
        ("ТГНЛ отпр.",      14.0),
        // Клиент / получатель
        ("Клиент",          28.0),
        ("ОКПО клиента",    18.0),
        ("Грузополучатель", 28.0),
        ("ОКПО грузополуч.",18.0),
        // Груз
        ("Груз ГНГ",        22.0),
        ("ЕТСНГ",           12.0),
        // Заявки
        ("Номера заявок",   22.0),
        ("Даты заявок",     22.0),
        ("№ ГУ-12",         18.0),
        // Вагоны
        ("Тип отправки",    16.0),
        ("Тип вагона",      12.0),
        ("Кол-во ваг.",     12.0),
        ("Ваг. на станции", 14.0),
    ];

    for (col, (title, width)) in headers.iter().enumerate() {
        ws.write_with_format(0, col as u16, *title, &hdr)?;
        ws.set_column_width(col as u16, *width)?;
    }

    // Вспомогательная функция: Vec<String> → "a | b | c"
    let join = |v: &Option<Vec<String>>| -> String {
        v.as_deref()
            .map(|s| s.join(" | "))
            .unwrap_or_default()
    };

    for (row_idx, n) in nodes.iter().enumerate() {
        let row = (row_idx + 1) as u32;

        macro_rules! s {
            ($v:expr) => { $v.as_deref().unwrap_or("") };
        }

        ws.write_with_format(row, 0,  n.d_id as u32,                     &num)?;
        ws.write_with_format(row, 1,  n.period,                          &num)?;
        ws.write_with_format(row, 2,  &n.station_name,                   &cell)?;
        ws.write_with_format(row, 3,  &n.station_code,                   &cell)?;
        ws.write_with_format(row, 4,  &n.railway_name,                   &cell)?;
        ws.write_with_format(row, 5,  s!(&n.railway_code),               &cell)?;
        ws.write_with_format(row, 6,  s!(&n.railway_part),               &cell)?;
        ws.write_with_format(row, 7,  s!(&n.station_to_name),            &cell)?;
        ws.write_with_format(row, 8,  s!(&n.station_to_code),            &cell)?;
        ws.write_with_format(row, 9,  s!(&n.railway_to_name),            &cell)?;
        ws.write_with_format(row, 10, s!(&n.railway_to_code),            &cell)?;
        ws.write_with_format(row, 11, s!(&n.railway_to_part),            &cell)?;
        ws.write_with_format(row, 12, s!(&n.sender),                     &cell)?;
        ws.write_with_format(row, 13, s!(&n.sender_okpo),                &cell)?;
        ws.write_with_format(row, 14, s!(&n.sender_tgnl),                &cell)?;
        ws.write_with_format(row, 15, join(&n.client),                   &cell)?;
        ws.write_with_format(row, 16, join(&n.customer_okpo),            &cell)?;
        ws.write_with_format(row, 17, join(&n.recipient),                &cell)?;
        ws.write_with_format(row, 18, join(&n.loader_to_okpo),           &cell)?;
        ws.write_with_format(row, 19, s!(&n.gng_cargo),                  &cell)?;
        ws.write_with_format(row, 20, s!(&n.etsng),                      &cell)?;
        ws.write_with_format(row, 21, join(&n.request_numbers),          &cell)?;
        ws.write_with_format(row, 22, join(&n.request_dates),            &cell)?;
        ws.write_with_format(row, 23, join(&n.gu12_number),              &cell)?;
        ws.write_with_format(row, 24, s!(&n.shipping_type),              &cell)?;
        ws.write_with_format(row, 25, s!(&n.car_type),                   &cell)?;
        ws.write_with_format(row, 26, n.car_count,                       &num)?;
        ws.write_with_format(row, 27, n.cars_on_station,                 &num)?;
    }

    // Автофильтр на всю таблицу
    ws.autofilter(0, 0, nodes.len() as u32, headers.len() as u16 - 1)?;

    // Закрепить первую строку
    ws.set_freeze_panes(1, 0)?;

    workbook.save(&path)?;
    Ok(path)
}

//! Дислокация вагонов (2–10 сутки): данные из Redis + MSSQL через Python (`dislocations.py`).

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde_json::Value;

use super::supply::supply_nodes_from_dislocation_json;

fn dislocations_script_path() -> Result<PathBuf> {
    Ok(
        std::env::current_dir()
            .context("не удалось получить текущую директорию")?
            .join("src/data/dislocations.py"),
    )
}

/// `DP.ShipmentGoalId` по номерам вагонов (режим `shipment_goals` скрипта).
/// Отсутствующий ключ или `null` в JSON — цель неизвестна (→ «По факту» в АПИ).
pub fn fetch_shipment_goals_for_car_numbers(car_numbers: &[u64]) -> Result<HashMap<u64, Option<i32>>> {
    let mut out: HashMap<u64, Option<i32>> = HashMap::new();
    if car_numbers.is_empty() {
        return Ok(out);
    }

    let script = dislocations_script_path()?;
    let payload = serde_json::to_string(car_numbers).context("serde JSON номеров вагонов")?;

    let mut child = Command::new("python3")
        .arg(&script)
        .arg("shipment_goals")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("не удалось запустить python3 {}", script.display()))?;

    child
        .stdin
        .as_mut()
        .context("stdin python")?
        .write_all(payload.as_bytes())
        .context("запись stdin shipment_goals")?;

    let output = child.wait_with_output().context("ожидание shipment_goals")?;

    if !output.status.success() {
        anyhow::bail!(
            "dislocations.py shipment_goals: {:?}\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout).context("stdout shipment_goals UTF-8")?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return Ok(out);
    }

    let v: Value = serde_json::from_str(trimmed).context("разбор JSON целей назначения")?;
    let Some(obj) = v.as_object() else {
        return Ok(out);
    };

    for (k, val) in obj {
        let Ok(car) = k.parse::<u64>() else {
            continue;
        };
        let gid = match val {
            Value::Null => None,
            Value::Number(n) => n.as_i64().map(|i| i as i32),
            _ => None,
        };
        out.insert(car, gid);
    }

    Ok(out)
}

/// Запускает `src/data/dislocations.py` (pymssql + redis), читает JSON массива вагонов со stdout.
pub fn fetch_dislocation_supply_nodes() -> Result<Vec<crate::node::SupplyNode>> {
    // let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/data/dislocations.py");
    // Получаем текущую директорию и ПРИСОЕДИНЯЕМ путь к файлу
    let script = dislocations_script_path()?;
    let output = Command::new("python3")
        .arg(&script)
        .output()
        .with_context(|| format!("не удалось запустить python3 и скрипт {}", script.display()))?;

    if !output.status.success() {
        anyhow::bail!(
            "dislocations.py завершился с кодом {:?}:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout).context("stdout dislocations.py не UTF-8")?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() || trimmed == "[]" {
        return Ok(Vec::new());
    }

    supply_nodes_from_dislocation_json(trimmed).map_err(|e| anyhow::anyhow!("JSON от dislocations.py: {e}"))
}

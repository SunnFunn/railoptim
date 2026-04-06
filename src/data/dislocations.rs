//! Дислокация вагонов (2–10 сутки): данные из Redis + MSSQL через Python (`dislocations.py`).

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use super::supply::supply_nodes_from_dislocation_json;

/// Запускает `src/data/dislocations.py` (pymssql + redis), читает JSON массива вагонов со stdout.
pub fn fetch_dislocation_supply_nodes() -> Result<Vec<crate::node::SupplyNode>> {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/data/dislocations.py");
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

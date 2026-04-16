use serde::Deserialize;

/// Ремонтная станция из словаря `repairs.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct RepairStation {
    #[serde(rename = "RepairRailWay")]
    pub railway: String,
    #[serde(rename = "RepairStationName")]
    pub station_name: String,
    #[serde(rename = "RepairStationCode")]
    pub station_code: String,
    #[serde(rename = "RecipName")]
    pub recip_name: Vec<String>,
    #[serde(rename = "RecipOKPO")]
    pub recip_okpo: Vec<String>,
}

/// Загружает список ремонтных станций из JSON-файла.
pub fn load_repair_stations(path: &str) -> anyhow::Result<Vec<RepairStation>> {
    let json = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Не удалось прочитать {}: {}", path, e))?;
    let stations = serde_json::from_str::<Vec<RepairStation>>(&json)
        .map_err(|e| anyhow::anyhow!("Ошибка разбора {}: {}", path, e))?;
    Ok(stations)
}

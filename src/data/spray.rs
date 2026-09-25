//! Распыление излишка: перспективный спрос следующего месяца и кластеры
//! вокруг станций из `data/spray_stations.json`.
//!
//! Правило включается только после 20-го числа: спрос — заявки со статусом
//! «Предварительный» на 1–20 число следующего месяца (MSSQL SLP, `prospective_demand.py`).
//! Станция погрузки попадает в кластер ближайшей станции распыления, если тарифное
//! расстояние не больше [`SPRAY_CLUSTER_RADIUS_KM`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use chrono::{Datelike, NaiveDate};
use serde::Deserialize;

use crate::node::TariffNode;

use super::esr::normalize_esr6;

/// Радиус кластера погрузки вокруг станции распыления, км.
pub const SPRAY_CLUSTER_RADIUS_KM: i32 = 1000;

pub const DEFAULT_SPRAY_STATIONS_PATH: &str = "data/spray_stations.json";

/// Станция распыления из справочника.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SprayStation {
    #[serde(rename = "rail_road")]
    pub railway: String,
    #[serde(rename = "station_name")]
    pub station_name: String,
    #[serde(rename = "station_code")]
    pub station_code: String,
    #[serde(rename = "station_capacity")]
    pub station_capacity: i32,
}

/// Станция перспективной погрузки (сумма вагонов по всем грузам).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProspectiveStation {
    pub railway: String,
    pub station_name: String,
    pub station_code: String,
    pub cars: i32,
}

/// Станция погрузки, вошедшая в кластер.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterMember {
    pub railway: String,
    pub station_name: String,
    pub station_code: String,
    pub cars: i32,
    pub distance_km: i32,
}

/// Кластер погрузки вокруг одной станции распыления.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SprayCluster {
    pub spray: SprayStation,
    /// Суммарный перспективный спрос станций кластера, вагонов.
    pub load_cars: i32,
    pub members: Vec<ClusterMember>,
}

impl SprayCluster {
    /// Сколько вагонов излишка можно отправить: не больше спроса кластера и вместимости станции.
    pub fn assignment_capacity(&self) -> i32 {
        self.load_cars.max(0).min(self.spray.station_capacity.max(0))
    }
}

#[derive(Debug, Deserialize)]
struct DemandRow {
    #[serde(default)]
    railway: String,
    #[serde(default)]
    station_name: String,
    #[serde(default)]
    station_code: String,
    #[serde(default)]
    cars: i32,
}

/// Окно 1–20 следующего месяца, если сегодня после 20-го числа. Иначе правило выключено.
pub fn spray_window(today: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    if today.day() <= 20 {
        return None;
    }
    let (year, month) = if today.month() == 12 {
        (today.year() + 1, 1)
    } else {
        (today.year(), today.month() + 1)
    };
    let start = NaiveDate::from_ymd_opt(year, month, 1)?;
    let end = NaiveDate::from_ymd_opt(year, month, 20)?;
    Some((start, end))
}

pub fn load_spray_stations(path: impl AsRef<std::path::Path>) -> Result<Vec<SprayStation>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("чтение {}", path.display()))?;
    let mut stations: Vec<SprayStation> = serde_json::from_str(&text)
        .with_context(|| format!("разбор {}", path.display()))?;
    for s in &mut stations {
        s.station_code = normalize_esr6(&s.station_code);
        s.railway = s.railway.trim().to_string();
        s.station_name = s.station_name.trim().to_string();
    }
    stations.retain(|s| s.station_code.len() == 6 && !s.railway.is_empty());
    Ok(stations)
}

fn script_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("текущая директория")?
        .join("src/data/prospective_demand.py"))
}

/// Предварительные заявки SLP за окно, свёрнутые по коду станции погрузки.
pub fn fetch_prospective_stations(from: NaiveDate, to: NaiveDate) -> Result<Vec<ProspectiveStation>> {
    let script = script_path()?;
    let output = Command::new("python3")
        .arg(&script)
        .arg("--from")
        .arg(from.format("%Y-%m-%d").to_string())
        .arg("--to")
        .arg(to.format("%Y-%m-%d").to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("запуск {}", script.display()))?;
    if !output.status.success() {
        bail!(
            "prospective_demand.py завершился с кодом {:?}",
            output.status.code()
        );
    }
    let rows: Vec<DemandRow> =
        serde_json::from_slice(&output.stdout).context("разбор JSON перспективного спроса")?;
    Ok(aggregate_stations(rows))
}

fn aggregate_stations(rows: Vec<DemandRow>) -> Vec<ProspectiveStation> {
    let mut by_code: HashMap<String, ProspectiveStation> = HashMap::new();
    for row in rows {
        let code = normalize_esr6(&row.station_code);
        if code.len() != 6 || row.cars <= 0 {
            continue;
        }
        let railway = row.railway.trim().to_string();
        let station_name = row.station_name.trim().to_string();
        let entry = by_code.entry(code.clone()).or_insert_with(|| ProspectiveStation {
            railway: railway.clone(),
            station_name: station_name.clone(),
            station_code: code,
            cars: 0,
        });
        if entry.railway.is_empty() {
            entry.railway = railway;
        }
        if entry.station_name.is_empty() {
            entry.station_name = station_name;
        }
        entry.cars += row.cars;
    }
    let mut stations: Vec<_> = by_code.into_values().collect();
    stations.sort_by(|a, b| a.station_code.cmp(&b.station_code));
    stations
}

/// Каждая станция погрузки — в ближайший кластер, если тарифное расстояние ≤ `radius_km`.
///
/// Ключ тарифа: `(код станции погрузки, код станции распыления)`.
/// При равном расстоянии побеждает меньший код станции распыления.
pub fn build_clusters(
    demand: &[ProspectiveStation],
    sprays: &[SprayStation],
    tariffs: &HashMap<(String, String), TariffNode>,
    radius_km: i32,
) -> Vec<SprayCluster> {
    let mut members: Vec<Vec<ClusterMember>> = vec![Vec::new(); sprays.len()];
    for d in demand {
        let mut best: Option<(i32, usize)> = None;
        for (i, spray) in sprays.iter().enumerate() {
            let Some(t) = tariffs.get(&(d.station_code.clone(), spray.station_code.clone())) else {
                continue;
            };
            if t.distance > radius_km {
                continue;
            }
            let better = match best {
                None => true,
                Some((dist, idx)) => t.distance < dist || (t.distance == dist && spray.station_code < sprays[idx].station_code),
            };
            if better {
                best = Some((t.distance, i));
            }
        }
        if let Some((distance_km, idx)) = best {
            members[idx].push(ClusterMember {
                railway: d.railway.clone(),
                station_name: d.station_name.clone(),
                station_code: d.station_code.clone(),
                cars: d.cars,
                distance_km,
            });
        }
    }
    sprays
        .iter()
        .zip(members)
        .filter(|(_, m)| !m.is_empty())
        .map(|(spray, mut members)| {
            members.sort_by(|a, b| a.station_code.cmp(&b.station_code));
            let load_cars = members.iter().map(|m| m.cars).sum();
            SprayCluster {
                spray: spray.clone(),
                load_cars,
                members,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn tariff(from: &str, to: &str, km: i32) -> ((String, String), TariffNode) {
        (
            (from.into(), to.into()),
            TariffNode {
                station_from: String::new(),
                station_from_code: from.into(),
                railway_from: String::new(),
                railway_from_code: 0,
                station_to: String::new(),
                station_to_code: to.into(),
                railway_to: String::new(),
                railway_to_code: 0,
                distance: km,
                period_of_delivery: 1,
                cost: km as f64,
                actual_date: NaiveDate::from_ymd_opt(2026, 6, 11)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            },
        )
    }

    fn spray(code: &str) -> SprayStation {
        SprayStation {
            railway: "ЮВС".into(),
            station_name: code.into(),
            station_code: code.into(),
            station_capacity: 70,
        }
    }

    fn demand(code: &str, cars: i32) -> ProspectiveStation {
        ProspectiveStation {
            railway: "ЮВС".into(),
            station_name: code.into(),
            station_code: code.into(),
            cars,
        }
    }

    #[test]
    fn window_closed_through_the_20th_and_opens_next_month() {
        assert_eq!(spray_window(day(2026, 9, 20)), None);
        assert_eq!(spray_window(day(2026, 9, 1)), None);
        assert_eq!(
            spray_window(day(2026, 9, 25)),
            Some((day(2026, 10, 1), day(2026, 10, 20)))
        );
        assert_eq!(
            spray_window(day(2026, 12, 21)),
            Some((day(2027, 1, 1), day(2027, 1, 20)))
        );
    }

    #[test]
    fn nearest_spray_within_radius_wins_tie_by_code() {
        let sprays = vec![spray("200000"), spray("100000")];
        let demand = vec![demand("300000", 12), demand("400000", 5)];
        let tariffs = HashMap::from([
            tariff("300000", "200000", 800),
            tariff("300000", "100000", 800),
            tariff("400000", "200000", 1001),
            tariff("400000", "100000", 400),
        ]);
        let clusters = build_clusters(&demand, &sprays, &tariffs, 1000);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].spray.station_code, "100000");
        assert_eq!(clusters[0].load_cars, 17);
        assert_eq!(clusters[0].assignment_capacity(), 17);
        assert_eq!(clusters[0].members.len(), 2);
    }

    #[test]
    fn catalog_loads_fifteen_stations_of_capacity_70() {
        let stations = load_spray_stations(DEFAULT_SPRAY_STATIONS_PATH).unwrap();
        assert_eq!(stations.len(), 15);
        assert!(stations.iter().all(|s| s.station_capacity == 70));
        assert!(stations.iter().any(|s| s.station_code == "830200"));
        assert!(stations.iter().any(|s| s.station_code == "597000"));
    }
}

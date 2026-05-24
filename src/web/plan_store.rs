//! Загрузка OptimReport с диска.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::solver::OptimReport;
use crate::web::config::WebConfig;

#[derive(Debug, Clone, Serialize)]
pub struct PlanSummary {
    pub plan_id: String,
    pub path: String,
    pub loaded_at: String,
    pub report_timestamp: String,
    pub solver_status: String,
    pub total_cost_rub: f64,
    pub assigned_cars: f64,
    pub assignment_count: usize,
}

#[derive(Debug, Clone)]
pub struct LoadedPlan {
    pub plan_id: String,
    pub path: PathBuf,
    pub loaded_at: DateTime<Utc>,
    pub report: OptimReport,
}

#[derive(Debug, Default)]
pub struct PlanStore {
    current: Option<LoadedPlan>,
    result_dir: PathBuf,
    explicit_file: Option<PathBuf>,
}

impl PlanStore {
    pub fn new(config: &WebConfig) -> Self {
        Self {
            current: None,
            result_dir: config.optim_result_dir.clone(),
            explicit_file: config.optim_result_file.clone(),
        }
    }

    pub fn reload(&mut self) -> Result<Option<&LoadedPlan>, std::io::Error> {
        let path = match self.resolve_path()? {
            Some(p) => p,
            None => {
                self.current = None;
                return Ok(None);
            }
        };

        let json = fs::read_to_string(&path)?;
        let report: OptimReport = serde_json::from_str(&json)?;
        let plan_id = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        self.current = Some(LoadedPlan {
            plan_id,
            path,
            loaded_at: Utc::now(),
            report,
        });
        Ok(self.current.as_ref())
    }

    pub fn current(&self) -> Option<&LoadedPlan> {
        self.current.as_ref()
    }

    pub fn summary(&self) -> Option<PlanSummary> {
        self.current.as_ref().map(|p| PlanSummary {
            plan_id: p.plan_id.clone(),
            path: p.path.display().to_string(),
            loaded_at: p.loaded_at.to_rfc3339(),
            report_timestamp: p.report.timestamp.clone(),
            solver_status: p.report.solver_status.clone(),
            total_cost_rub: p.report.total_cost_rub,
            assigned_cars: p.report.assigned_cars,
            assignment_count: p.report.assignments.len(),
        })
    }

    pub fn list_plan_files(&self) -> Result<Vec<PlanFileEntry>, std::io::Error> {
        let mut entries = Vec::new();
        if !self.result_dir.is_dir() {
            return Ok(entries);
        }

        for entry in fs::read_dir(&self.result_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with("result_") || !name.ends_with(".json") {
                continue;
            }
            let meta = entry.metadata()?;
            let modified = meta.modified().ok();
            entries.push(PlanFileEntry {
                plan_id: name.to_string(),
                path: path.display().to_string(),
                size_bytes: meta.len(),
                modified_at: modified.and_then(system_time_to_rfc3339),
            });
        }

        entries.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
        Ok(entries)
    }

    fn resolve_path(&self) -> Result<Option<PathBuf>, std::io::Error> {
        if let Some(path) = &self.explicit_file {
            if path.is_file() {
                return Ok(Some(path.clone()));
            }
            return Ok(None);
        }

        find_latest_result_file(&self.result_dir)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanFileEntry {
    pub plan_id: String,
    pub path: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
}

pub fn find_latest_result_file(dir: &Path) -> Result<Option<PathBuf>, std::io::Error> {
    if !dir.is_dir() {
        return Ok(None);
    }

    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("result_") || !name.ends_with(".json") {
            continue;
        }
        let modified = entry.metadata()?.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        match &best {
            Some((t, _)) if modified <= *t => {}
            _ => best = Some((modified, path)),
        }
    }

    Ok(best.map(|(_, p)| p))
}

fn system_time_to_rfc3339(t: SystemTime) -> Option<String> {
    let dt: DateTime<Utc> = t.into();
    Some(dt.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solver::AssignmentRecord;
    use std::thread;
    use std::time::Duration;

    fn sample_report() -> OptimReport {
        OptimReport {
            timestamp: "2026-01-01 12:00:00".into(),
            solver_status: "optimal".into(),
            total_cost_rub: 1000.0,
            assigned_cars: 2.0,
            penalty_cars: 0.0,
            supply_count: 1,
            demand_count: 1,
            arc_count: 1,
            assignments: vec![AssignmentRecord {
                cars: 2.0,
                supply_id: 1,
                supply_kind: "Free".into(),
                car_numbers: vec![123],
                supply_station: "A".into(),
                supply_station_code: "111111".into(),
                supply_railway: "RW".into(),
                demand_id: 1,
                demand_station: "B".into(),
                demand_station_code: "222222".into(),
                demand_railway: "RW".into(),
                demand_period: 1,
                cost_rub: 500.0,
                distance_km: 100,
                delivery_days: 3,
                period_ok: true,
                car_type_ok: true,
            }],
        }
    }

    #[test]
    fn find_latest_by_mtime() {
        let dir = std::env::temp_dir().join(format!("railoptim_plan_store_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let old_path = dir.join("result_20260101_100000.json");
        let new_path = dir.join("result_20260102_100000.json");
        fs::write(&old_path, "{}").unwrap();
        thread::sleep(Duration::from_millis(20));
        fs::write(&new_path, "{}").unwrap();

        let latest = find_latest_result_file(&dir).unwrap().unwrap();
        assert_eq!(latest, new_path);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reload_parses_report() {
        let dir = std::env::temp_dir().join(format!("railoptim_plan_reload_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let path = dir.join("result_test.json");
        fs::write(&path, serde_json::to_string(&sample_report()).unwrap()).unwrap();

        let config = WebConfig {
            bind_addr: "127.0.0.1:8080".parse().unwrap(),
            stations_geo_db: PathBuf::from("data/stations/stations_geo.sqlite"),
            optim_result_dir: dir.clone(),
            optim_result_file: Some(path),
            cors_origins: vec!["*".into()],
        };

        let mut store = PlanStore::new(&config);
        let loaded = store.reload().unwrap().expect("plan loaded");
        assert_eq!(loaded.report.assigned_cars, 2.0);
        assert_eq!(loaded.report.assignments.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }
}

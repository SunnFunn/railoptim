//! Обогащение назначений координатами из StationGeoCatalog.

use std::collections::{HashMap, HashSet};

use crate::data::StationGeoCatalog;
use crate::solver::AssignmentRecord;
use crate::web::dto::{MapArc, MapFiltersMeta, MapGeoEndpoint, MapNode, MapStats, PlanMapResponse};
use crate::web::plan_store::{LoadedPlan, PlanSummary};

pub fn build_map_response(
    plan: &LoadedPlan,
    summary: PlanSummary,
    catalog: &StationGeoCatalog,
) -> PlanMapResponse {
    let mut arcs = Vec::with_capacity(plan.report.assignments.len());
    let mut arcs_resolved = 0usize;
    let mut arcs_missing_geo = 0usize;
    let mut supply_railways = HashSet::new();
    let mut demand_railways = HashSet::new();

    for (id, assignment) in plan.report.assignments.iter().enumerate() {
        supply_railways.insert(assignment.supply_railway.clone());
        demand_railways.insert(assignment.demand_railway.clone());

        let (from, to, geo_status) = map_arc_endpoints(assignment, catalog);
        if geo_status == "ok" {
            arcs_resolved += 1;
        } else {
            arcs_missing_geo += 1;
        }
        arcs.push(MapArc {
            id,
            from,
            to,
            cars: assignment.cars,
            distance_km: assignment.distance_km,
            cost_rub: assignment.cost_rub,
            supply_kind: assignment.supply_kind.clone(),
            supply_railway: assignment.supply_railway.clone(),
            demand_railway: assignment.demand_railway.clone(),
            demand_period: assignment.demand_period,
            geo_status,
        });
    }

    let mut supply_list: Vec<_> = supply_railways.into_iter().collect();
    supply_list.sort();
    let mut demand_list: Vec<_> = demand_railways.into_iter().collect();
    demand_list.sort();

    let nodes = aggregate_nodes(&plan.report.assignments, catalog);
    let stats = MapStats {
        arcs_total: arcs.len(),
        arcs_resolved,
        arcs_missing_geo,
        nodes_total: nodes.len(),
    };

    PlanMapResponse {
        plan_id: plan.plan_id.clone(),
        summary,
        stats,
        filters: MapFiltersMeta {
            supply_railways: supply_list,
            demand_railways: demand_list,
        },
        arcs,
        nodes,
    }
}

fn map_arc_endpoints(
    assignment: &AssignmentRecord,
    catalog: &StationGeoCatalog,
) -> (MapGeoEndpoint, MapGeoEndpoint, &'static str) {
    let from = endpoint(&assignment.supply_station_code, &assignment.supply_station, catalog);
    let to = endpoint(&assignment.demand_station_code, &assignment.demand_station, catalog);

    let geo_status = if from.lat.is_some() && from.lon.is_some() && to.lat.is_some() && to.lon.is_some() {
        "ok"
    } else {
        "missing"
    };

    (from, to, geo_status)
}

fn endpoint(esr6: &str, fallback_name: &str, catalog: &StationGeoCatalog) -> MapGeoEndpoint {
    match catalog.get(esr6) {
        Some(st) => MapGeoEndpoint {
            esr6: st.esr6.clone(),
            name: st.name.clone(),
            lat: Some(st.lat),
            lon: Some(st.lon),
        },
        None => MapGeoEndpoint {
            esr6: esr6.to_string(),
            name: fallback_name.to_string(),
            lat: None,
            lon: None,
        },
    }
}

#[derive(Default)]
struct NodeAcc {
    name: String,
    lat: Option<f64>,
    lon: Option<f64>,
    supply_cars: f64,
    demand_cars: f64,
}

fn aggregate_nodes(assignments: &[AssignmentRecord], catalog: &StationGeoCatalog) -> Vec<MapNode> {
    let mut by_esr: HashMap<String, NodeAcc> = HashMap::new();

    for a in assignments {
        accumulate(
            &mut by_esr,
            &a.supply_station_code,
            &a.supply_station,
            catalog,
            a.cars,
            true,
        );
        accumulate(
            &mut by_esr,
            &a.demand_station_code,
            &a.demand_station,
            catalog,
            a.cars,
            false,
        );
    }

    let mut nodes: Vec<MapNode> = by_esr
        .into_iter()
        .filter_map(|(esr6, acc)| {
            let (lat, lon) = match (acc.lat, acc.lon) {
                (Some(lat), Some(lon)) => (lat, lon),
                _ => return None,
            };
            let (role, cars_total) = if acc.supply_cars > 0.0 && acc.demand_cars > 0.0 {
                ("both", acc.supply_cars + acc.demand_cars)
            } else if acc.supply_cars > 0.0 {
                ("supply", acc.supply_cars)
            } else {
                ("demand", acc.demand_cars)
            };
            Some(MapNode {
                esr6,
                name: acc.name,
                lat,
                lon,
                role,
                cars_total,
            })
        })
        .collect();

    nodes.sort_by(|a, b| a.esr6.cmp(&b.esr6));
    nodes
}

fn accumulate(
    by_esr: &mut HashMap<String, NodeAcc>,
    esr6: &str,
    fallback_name: &str,
    catalog: &StationGeoCatalog,
    cars: f64,
    is_supply: bool,
) {
    let key = crate::data::normalize_esr6(esr6);
    let entry = by_esr.entry(key).or_default();
    if entry.name.is_empty() {
        entry.name = catalog
            .get(esr6)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| fallback_name.to_string());
    }
    if entry.lat.is_none() {
        if let Some(st) = catalog.get(esr6) {
            entry.lat = Some(st.lat);
            entry.lon = Some(st.lon);
        }
    }
    if is_supply {
        entry.supply_cars += cars;
    } else {
        entry.demand_cars += cars;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solver::{AssignmentRecord, OptimReport};
    use crate::web::plan_store::LoadedPlan;
    use chrono::Utc;
    use std::path::PathBuf;

    fn sample_assignment(from: &str, to: &str, supply_rw: &str, demand_rw: &str) -> AssignmentRecord {
        AssignmentRecord {
            cars: 1.0,
            supply_id: 1,
            supply_kind: "Free".into(),
            car_numbers: vec![],
            supply_station: "From".into(),
            supply_station_code: from.into(),
            supply_railway: supply_rw.into(),
            demand_id: 1,
            demand_station: "To".into(),
            demand_station_code: to.into(),
            demand_railway: demand_rw.into(),
            demand_period: 1,
            cost_rub: 100.0,
            distance_km: 50,
            delivery_days: 2,
            period_ok: true,
            car_type_ok: true,
        }
    }

    #[test]
    fn enrich_resolves_coords() {
        use std::fs;
        use rusqlite::Connection;

        let dir = std::env::temp_dir().join(format!("railoptim_geo_enrich_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("geo.sqlite");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE stations_geo (
              esr6 TEXT PRIMARY KEY, name TEXT, lat REAL, lon REAL,
              country_hint TEXT, region_group TEXT, source TEXT, match_method TEXT,
              osm_id INTEGER, name_osm TEXT, confidence REAL, built_at TEXT);
             INSERT INTO stations_geo VALUES
              ('111111','A',55.0,37.0,NULL,'ru','t','m',NULL,NULL,1.0,'x'),
              ('222222','B',56.0,38.0,NULL,'ru','t','m',NULL,NULL,1.0,'x');",
        )
        .unwrap();

        let catalog = StationGeoCatalog::load(&db).unwrap();
        let report = OptimReport {
            timestamp: "t".into(),
            solver_status: "ok".into(),
            total_cost_rub: 100.0,
            assigned_cars: 1.0,
            penalty_cars: 0.0,
            supply_count: 1,
            demand_count: 1,
            arc_count: 1,
            assignments: vec![sample_assignment("111111", "222222", "Московская", "Октябрьская")],
        };
        let plan = LoadedPlan {
            plan_id: "test.json".into(),
            path: PathBuf::from("test.json"),
            loaded_at: Utc::now(),
            report,
        };
        let summary = PlanSummary {
            plan_id: "test.json".into(),
            path: "test.json".into(),
            loaded_at: Utc::now().to_rfc3339(),
            report_timestamp: "t".into(),
            solver_status: "ok".into(),
            total_cost_rub: 100.0,
            assigned_cars: 1.0,
            assignment_count: 1,
        };

        let map = build_map_response(&plan, summary, &catalog);
        assert_eq!(map.stats.arcs_resolved, 1);
        assert_eq!(map.stats.arcs_missing_geo, 0);
        assert_eq!(map.nodes.len(), 2);
        assert_eq!(map.arcs[0].supply_railway, "Московская");
        assert_eq!(map.arcs[0].demand_railway, "Октябрьская");
        assert_eq!(map.filters.supply_railways, vec!["Московская"]);
        assert_eq!(map.filters.demand_railways, vec!["Октябрьская"]);

        let _ = fs::remove_dir_all(&dir);
    }
}

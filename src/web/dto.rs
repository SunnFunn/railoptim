//! DTO для JSON API (deck.gl / MapLibre).

use serde::Serialize;

use crate::solver::OptimReport;
use crate::web::plan_store::PlanSummary;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct MetaResponse {
    pub service: &'static str,
    pub version: &'static str,
    pub stations_geo_count: usize,
    pub stations_geo_path: String,
    pub optim_result_dir: String,
    pub plan: Option<PlanSummary>,
}

#[derive(Debug, Serialize)]
pub struct StationResponse {
    pub esr6: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub country_hint: Option<String>,
    pub region_group: Option<String>,
    pub source: String,
    pub match_method: String,
    pub confidence: f64,
}

#[derive(Debug, Serialize)]
pub struct PlanListResponse {
    pub plans: Vec<crate::web::plan_store::PlanFileEntry>,
    pub loaded: Option<PlanSummary>,
}

#[derive(Debug, Serialize)]
pub struct PlanLatestResponse {
    pub plan: PlanSummary,
    pub report: OptimReport,
}

#[derive(Debug, Serialize)]
pub struct MapGeoPoint {
    pub esr6: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Serialize)]
pub struct MapGeoEndpoint {
    pub esr6: String,
    pub name: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct MapArc {
    pub id: usize,
    pub from: MapGeoEndpoint,
    pub to: MapGeoEndpoint,
    pub cars: f64,
    pub distance_km: i32,
    pub cost_rub: f64,
    pub supply_kind: String,
    pub supply_railway: String,
    pub demand_railway: String,
    /// `1` — 1-е сутки; `10` — дислокация 2–10 суток.
    pub supply_period: u8,
    pub demand_period: u8,
    pub geo_status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct MapNode {
    pub esr6: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub role: &'static str,
    pub cars_total: f64,
}

#[derive(Debug, Serialize)]
pub struct MapStats {
    pub arcs_total: usize,
    pub arcs_resolved: usize,
    pub arcs_missing_geo: usize,
    pub nodes_total: usize,
}

#[derive(Debug, Serialize)]
pub struct MapFiltersMeta {
    pub supply_railways: Vec<String>,
    pub demand_railways: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PlanMapResponse {
    pub plan_id: String,
    pub summary: PlanSummary,
    pub stats: MapStats,
    pub filters: MapFiltersMeta,
    pub arcs: Vec<MapArc>,
    pub nodes: Vec<MapNode>,
}

#[derive(Debug, Serialize)]
pub struct ReloadResponse {
    pub reloaded: bool,
    pub plan: Option<PlanSummary>,
}

#[derive(Debug, Serialize)]
pub struct GeoReloadResponse {
    pub reloaded: bool,
    pub stations_geo_count: usize,
    pub stations_geo_path: String,
}

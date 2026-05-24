use axum::extract::State;
use axum::Json;

use crate::web::dto::{PlanLatestResponse, PlanListResponse, PlanMapResponse, ReloadResponse};
use crate::web::error::ApiError;
use crate::web::geo_enrich::build_map_response;
use crate::web::state::AppState;

pub async fn list_plans(State(state): State<AppState>) -> Result<Json<PlanListResponse>, ApiError> {
    let plans = state.plans.read().await;
    let entries = plans.list_plan_files()?;
    Ok(Json(PlanListResponse {
        plans: entries,
        loaded: plans.summary(),
    }))
}

pub async fn latest_plan(State(state): State<AppState>) -> Result<Json<PlanLatestResponse>, ApiError> {
    let plans = state.plans.read().await;
    let loaded = plans
        .current()
        .ok_or_else(|| ApiError::NotFound("план назначений не загружен".into()))?;
    let summary = plans
        .summary()
        .ok_or_else(|| ApiError::Internal("plan summary unavailable".into()))?;

    Ok(Json(PlanLatestResponse {
        plan: summary,
        report: loaded.report.clone(),
    }))
}

pub async fn latest_plan_map(State(state): State<AppState>) -> Result<Json<PlanMapResponse>, ApiError> {
    let plans = state.plans.read().await;
    let loaded = plans
        .current()
        .ok_or_else(|| ApiError::NotFound("план назначений не загружен".into()))?;
    let summary = plans
        .summary()
        .ok_or_else(|| ApiError::Internal("plan summary unavailable".into()))?;
    let stations = state.stations.read().await;

    Ok(Json(build_map_response(
        loaded,
        summary,
        &stations,
    )))
}

pub async fn reload_plan(State(state): State<AppState>) -> Result<Json<ReloadResponse>, ApiError> {
    let mut plans = state.plans.write().await;
    plans.reload()?;
    Ok(Json(ReloadResponse {
        reloaded: true,
        plan: plans.summary(),
    }))
}

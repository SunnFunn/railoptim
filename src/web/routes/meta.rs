use axum::extract::State;
use axum::Json;

use crate::web::dto::MetaResponse;
use crate::web::state::AppState;

pub async fn meta(State(state): State<AppState>) -> Json<MetaResponse> {
    let plans = state.plans.read().await;
    let stations = state.stations.read().await;
    Json(MetaResponse {
        service: "railoptim-web",
        version: env!("CARGO_PKG_VERSION"),
        stations_geo_count: stations.len(),
        stations_geo_path: stations.path().display().to_string(),
        optim_result_dir: state.config.optim_result_dir.display().to_string(),
        plan: plans.summary(),
    })
}

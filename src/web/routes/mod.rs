mod health;
mod meta;
mod plans;
mod stations;

#[cfg(test)]
mod api_tests;

use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::web::state::AppState;

pub fn router(state: AppState) -> Router {
    let cors = build_cors(&state.config.cors_origins);

    let api = Router::new()
        .route("/health", get(health::health))
        .route("/api/v1/meta", get(meta::meta))
        .route(
            "/api/v1/stations/reload",
            get(stations::reload_stations_hint).post(stations::reload_stations),
        )
        .route("/api/v1/stations/{esr6}", get(stations::get_station))
        .route("/api/v1/plans", get(plans::list_plans))
        .route("/api/v1/plans/latest", get(plans::latest_plan))
        .route("/api/v1/plans/latest/map", get(plans::latest_plan_map))
        .route("/api/v1/plans/reload", post(plans::reload_plan))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    if let Some(static_dir) = &state.config.static_dir {
        let index = static_dir.join("index.html");
        if index.is_file() {
            let serve = ServeDir::new(static_dir)
                .not_found_service(ServeFile::new(index));
            return Router::new().merge(api).fallback_service(serve);
        }
        tracing::warn!(
            path = %static_dir.display(),
            "WEB_STATIC_DIR set but index.html missing — API only"
        );
    }

    api
}

fn build_cors(origins: &[String]) -> CorsLayer {
    if origins.len() == 1 && origins[0] == "*" {
        return CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);
    }

    let allowed: Vec<_> = origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();

    CorsLayer::new()
        .allow_origin(allowed)
        .allow_methods(Any)
        .allow_headers(Any)
}

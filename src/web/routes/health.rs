use axum::Json;

use crate::web::dto::HealthResponse;

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

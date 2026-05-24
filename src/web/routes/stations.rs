use axum::extract::{Path, State};
use axum::Json;

use crate::web::dto::StationResponse;
use crate::web::error::ApiError;
use crate::web::state::AppState;

pub async fn get_station(
    State(state): State<AppState>,
    Path(esr6): Path<String>,
) -> Result<Json<StationResponse>, ApiError> {
    let stations = state.stations.read().await;
    let station = stations.get(&esr6).ok_or_else(|| {
        ApiError::NotFound(format!(
            "станция esr6={esr6} не найдена в каталоге ({} записей, файл: {}). \
             Если станция только что добавлена через build-geo, выполните \
             POST /api/v1/stations/reload или перезапустите railoptim-web",
            stations.len(),
            stations.path().display()
        ))
    })?;

    Ok(Json(StationResponse {
        esr6: station.esr6.clone(),
        name: station.name.clone(),
        lat: station.lat,
        lon: station.lon,
        country_hint: station.country_hint.clone(),
        region_group: station.region_group.clone(),
        source: station.source.clone(),
        match_method: station.match_method.clone(),
        confidence: station.confidence,
    }))
}

pub async fn reload_stations(
    State(state): State<AppState>,
) -> Result<Json<crate::web::dto::GeoReloadResponse>, ApiError> {
    let catalog = state.reload_stations().await?;
    Ok(Json(crate::web::dto::GeoReloadResponse {
        reloaded: true,
        stations_geo_count: catalog.len(),
        stations_geo_path: catalog.path().display().to_string(),
    }))
}

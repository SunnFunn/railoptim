//! Shared application state.

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::data::StationGeoCatalog;
use crate::web::config::WebConfig;
use crate::web::plan_store::PlanStore;

#[derive(Clone)]
pub struct AppState {
    pub stations: Arc<RwLock<StationGeoCatalog>>,
    pub plans: Arc<RwLock<PlanStore>>,
    pub config: WebConfig,
}

impl AppState {
    pub fn new(config: WebConfig, stations: StationGeoCatalog) -> Result<Self, std::io::Error> {
        let mut plan_store = PlanStore::new(&config);
        plan_store.reload()?;
        Ok(Self {
            stations: Arc::new(RwLock::new(stations)),
            plans: Arc::new(RwLock::new(plan_store)),
            config,
        })
    }

    /// Перечитать `stations_geo.sqlite` с диска (после `build-geo`).
    pub async fn reload_stations(&self) -> Result<StationGeoCatalog, crate::data::StationGeoError> {
        let path = self.config.stations_geo_db.clone();
        let catalog = StationGeoCatalog::load(&path)?;
        let mut guard = self.stations.write().await;
        *guard = catalog.clone();
        Ok(catalog)
    }
}

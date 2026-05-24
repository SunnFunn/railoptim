//! Shared application state.

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::data::StationGeoCatalog;
use crate::web::config::WebConfig;
use crate::web::plan_store::PlanStore;

#[derive(Clone)]
pub struct AppState {
    pub stations: Arc<StationGeoCatalog>,
    pub plans: Arc<RwLock<PlanStore>>,
    pub config: WebConfig,
}

impl AppState {
    pub fn new(config: WebConfig, stations: StationGeoCatalog) -> Result<Self, std::io::Error> {
        let mut plan_store = PlanStore::new(&config);
        plan_store.reload()?;
        Ok(Self {
            stations: Arc::new(stations),
            plans: Arc::new(RwLock::new(plan_store)),
            config,
        })
    }
}

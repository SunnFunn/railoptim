use anyhow::Context;
use railoptim::data::StationGeoCatalog;
use railoptim::web::{init_tracing, serve, AppState, WebConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let config = WebConfig::from_env().context("web config")?;
    if !config.stations_geo_db.is_file() {
        anyhow::bail!(
            "stations geo db not found: {}",
            config.stations_geo_db.display()
        );
    }

    let stations = StationGeoCatalog::load(&config.stations_geo_db)
        .with_context(|| format!("load stations geo from {}", config.stations_geo_db.display()))?;

    if stations.is_empty() {
        anyhow::bail!("stations geo catalog is empty");
    }

    info_startup(&config, &stations);

    let state = AppState::new(config, stations)?;
    serve(state).await
}

fn info_startup(config: &WebConfig, stations: &StationGeoCatalog) {
    let serving_spa = config
        .static_dir
        .as_ref()
        .map(|d| d.join("index.html").is_file())
        .unwrap_or(false);

    tracing::info!(
        bind = %config.bind_addr,
        stations = stations.len(),
        stations_db = %stations.path().display(),
        result_dir = %config.optim_result_dir.display(),
        result_file = config.optim_result_file.as_ref().map(|p| p.display().to_string()),
        static_dir = config.static_dir.as_ref().map(|p| p.display().to_string()),
        serving_spa,
        "starting railoptim-web"
    );
}

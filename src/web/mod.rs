mod config;
mod dto;
mod error;
mod geo_enrich;
mod plan_store;
mod routes;
mod state;

use axum::serve as axum_serve;
use tokio::net::TcpListener;
use tracing::info;

pub use config::{WebConfig, WebConfigError};
pub use state::AppState;

pub async fn serve(state: AppState) -> anyhow::Result<()> {
    let addr = state.config.bind_addr;
    let router = routes::router(state);
    let listener = TcpListener::bind(addr).await?;
    info!("railoptim-web listening on http://{addr}");
    axum_serve(listener, router).await?;
    Ok(())
}

pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "railoptim_web=info,tower_http=info,axum=info".into()),
        )
        .init();
}

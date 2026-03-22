mod api;
mod config;
mod hsolver;
mod node;

use anyhow::Result;
use api::ApiClient;
use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Config::from_env()?;
    let client = ApiClient::new(&cfg.api_base_url, &cfg.api_token)?;

    let demand_nodes = client.fetch_demand_nodes().await?;
    println!("Получено узлов спроса: {}", demand_nodes.len());

    Ok(())
}

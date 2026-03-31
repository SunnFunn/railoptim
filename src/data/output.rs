use crate::solver::result::OutputRecord;
use super::client::{ApiClient, ApiEndpoint, ApiError};

impl ApiClient {
    /// Отправляет план назначений в АПИ (POST DestinationRegistryTransmission).
    ///
    /// Тело запроса — массив JSON, соответствующий схеме `request.json`.
    /// Возвращает `Ok(())` при статусе 2xx.
    pub async fn send_assignments(&self, records: &[OutputRecord]) -> Result<(), ApiError> {
        let url = ApiEndpoint::Output.url(&self.base_url);

        let response = self
            .client
            .post(&url)
            .json(records)
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::UnexpectedStatus { status: status.as_u16(), body });
        }

        Ok(())
    }
}

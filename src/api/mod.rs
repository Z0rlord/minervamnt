pub mod ark;
pub mod cashu;

use crate::ark_client::MockArkClient;
use crate::health::{collect_health, HealthState};
use crate::mint_backend::MintBackend;
use crate::AppConfig;
use axum::routing::get;
use axum::Router;
use std::sync::Arc;

pub type AppState = HealthState<MockArkClient>;

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(cashu::routes())
        .merge(ark::routes())
        .route("/health", get(health_handler))
        .with_state(Arc::new(state))
}

async fn health_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::Json<crate::health::HealthStatus> {
    axum::Json(collect_health(&state.config, &state.backend).await)
}

pub fn build_state(config: AppConfig, backend: Arc<MintBackend<MockArkClient>>) -> AppState {
    HealthState {
        config: Arc::new(config),
        backend,
    }
}

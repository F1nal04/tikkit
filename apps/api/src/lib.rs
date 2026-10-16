use axum::{Router, routing::get};

pub mod config;
pub mod db;
pub mod health;
pub mod server;
pub mod state;

pub use config::Config;
pub use state::AppState;

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health::live))
        .route("/api/health/live", get(health::live))
        .route("/api/health/ready", get(health::ready))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    use crate::{AppState, Config, db};

    /// State whose pool never connects (no database needed).
    fn offline_state() -> AppState {
        let config = Config::from_source(|name| {
            (name == "DATABASE_URL")
                .then(|| "postgres://tikkit:tikkit@127.0.0.1:1/tikkit".to_owned())
        })
        .unwrap();
        AppState::new(db::connect_lazy(&config), config)
    }

    #[tokio::test]
    async fn health_returns_json_status() {
        for path in ["/api/health", "/api/health/live"] {
            let response = super::app(offline_state())
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["content-type"], "application/json");
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(payload, serde_json::json!({ "status": "ok" }));
        }
    }
}

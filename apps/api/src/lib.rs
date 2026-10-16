use axum::{Router, middleware, routing::get};

pub mod auth;
pub mod config;
pub mod db;
pub mod domain;
pub mod health;
pub mod server;
pub mod state;

// HTTP contract modules (issue #7): errors, extractors, pagination, request
// ids, OpenAPI, shared DTOs. See docs/api.md.
pub mod dto;
pub mod error;
pub mod extract;
pub mod http;
pub mod openapi;
pub mod pagination;
pub mod request_id;

pub use config::Config;
pub use state::AppState;

pub fn app(state: AppState) -> Router {
    let router = Router::new()
        .route("/api/health", get(health::live))
        .route("/api/health/live", get(health::live_alias))
        .route("/api/health/ready", get(health::ready))
        .route("/api/openapi.json", get(openapi::serve))
        .merge(auth::handlers::routes())
        // Session cookie slot (innermost), then the CSRF origin check, so a
        // rejected request never touches sessions.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::cookie::middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::csrf::middleware,
        ))
        .with_state(state);
    // Shared HTTP contract (fallbacks, body limit, request ids). Keep this
    // the last step after all routes are added, whatever the router state.
    http::apply_api_layers(router)
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

    #[tokio::test]
    async fn serves_openapi_document() {
        let response = super::app(offline_state())
            .oneshot(
                Request::get("/api/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key("x-request-id"));
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let served: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let generated: serde_json::Value =
            serde_json::from_str(&crate::openapi::spec_json()).unwrap();
        assert_eq!(served, generated);
    }
}

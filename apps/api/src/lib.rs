use axum::{Json, Router, routing::get};
use serde::Serialize;
use sqlx::PgPool;

pub mod db;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/health",
            get(|| async { Json(Health { status: "ok" }) }),
        )
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

    pub fn lazy_state() -> super::AppState {
        // Never connects unless a handler touches the database.
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://tikkit@127.0.0.1:1/tikkit")
            .unwrap();
        super::AppState { pool }
    }

    #[tokio::test]
    async fn health_returns_json_status() {
        let response = super::app(lazy_state())
            .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "application/json");
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload, serde_json::json!({ "status": "ok" }));
    }
}

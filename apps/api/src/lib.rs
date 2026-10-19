use std::{path::Path, sync::Arc, time::Duration};

use axum::{
    Extension, Router,
    extract::{DefaultBodyLimit, State},
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::{get, patch, post, put},
};
use serde::Serialize;
use sqlx::PgPool;
use tower_http::{
    catch_panic::CatchPanicLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::{DefaultOnResponse, TraceLayer},
};
use utoipa::{OpenApi, ToSchema};

pub mod accounts;
pub mod api;
pub mod assignment;
pub mod auth;
pub mod authz;
pub mod catalog;
pub mod config;
pub mod db;
pub mod lifecycle;
pub mod mail;
pub mod management;
pub mod messages;
pub mod queues;
pub mod screenshots;
pub mod smtp;
pub mod submission;
pub mod tickets;

use api::{ApiError, Json};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    /// Private screenshot volume (`SCREENSHOT_DIR`); never served directly.
    pub screenshot_dir: Arc<Path>,
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Tikkit API",
        version = "1",
        description = "Conventions: docs/api-conventions.md. Errors use the ErrorEnvelope schema; \
                       lists use `limit`/`cursor` and return a Page. Authentication is the \
                       HttpOnly `tikkit_session` cookie; every POST/PUT/PATCH/DELETE must send \
                       `X-Requested-With: tikkit` or is rejected with 403 `csrf_rejected` (docs/auth.md)."
    ),
    paths(
        health,
        ready,
        openapi_json,
        auth::setup_status,
        auth::setup,
        auth::login,
        auth::logout,
        auth::me,
        auth::change_password,
        accounts::update_profile,
        accounts::request_recovery,
        accounts::reset_password,
        accounts::create_invitation,
        accounts::list_invitations,
        accounts::revoke_invitation,
        accounts::lookup_invitation,
        accounts::accept_invitation,
        accounts::list_accounts,
        accounts::deactivate_account,
        accounts::reactivate_account,
        accounts::reissue_invitation,
        management::update_account,
        management::list_teams,
        management::create_team,
        management::update_team,
        management::organization,
        management::settings,
        management::update_settings,
        smtp::get_smtp,
        smtp::put_smtp,
        smtp::delete_smtp,
        smtp::test_smtp,
        catalog::active_catalog,
        catalog::full_catalog,
        catalog::create_category,
        catalog::update_category,
        catalog::reorder_categories,
        catalog::create_priority,
        catalog::update_priority,
        catalog::reorder_priorities,
        catalog::create_intake_field,
        catalog::update_intake_field,
        catalog::reorder_intake_fields,
        tickets::ticket_detail,
        tickets::list_own_tickets,
        tickets::requester_action,
        tickets::staff_action,
        tickets::ticket_activity,
        assignment::change_assignment,
        queues::list_queue,
        queues::queue_counts,
        messages::list,
        messages::reply,
        messages::note,
        submission::create_draft,
        submission::list_drafts,
        submission::get_draft,
        submission::update_draft,
        submission::delete_draft,
        submission::submit_ticket,
        submission::submit_draft,
        screenshots::upload,
        screenshots::metadata,
        screenshots::preview,
        screenshots::download,
        screenshots::delete,
    ),
    components(schemas(
        api::ErrorEnvelope,
        api::Page<api::TicketAssignmentView>,
        api::Language,
        api::Role,
        api::TicketStatus,
        api::TicketAssignmentView,
        queues::QueueView,
        queues::QueueSort,
    ))
)]
pub struct ApiDoc;

pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}

#[derive(Serialize, ToSchema)]
struct Health {
    status: &'static str,
}

#[derive(Serialize, ToSchema)]
struct Readiness {
    /// `ready` or `unavailable`.
    status: &'static str,
    checks: Checks,
}

#[derive(Serialize, ToSchema)]
struct Checks {
    /// `ok` or `unavailable`.
    database: &'static str,
    /// `ok`, `pending`, `incompatible`, or `unknown`.
    migrations: &'static str,
}

/// Liveness: the process is serving. Touches no dependencies.
#[utoipa::path(get, path = "/api/health", tag = "operations",
    responses((status = 200, description = "Process is alive", body = Health)))]
async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

/// Readiness: the database answers and its schema matches this build.
/// Reports stable check identifiers only, never connection details.
#[utoipa::path(get, path = "/api/ready", tag = "operations",
    responses(
        (status = 200, description = "Ready to serve traffic", body = Readiness),
        (status = 503, description = "A required dependency is unavailable or incompatible", body = Readiness),
    ))]
async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Readiness>) {
    let probe = async {
        sqlx::query("SELECT 1").execute(&state.pool).await?;
        db::migration_status(&state.pool).await
    };
    let checks = match tokio::time::timeout(Duration::from_secs(3), probe).await {
        Ok(Ok(migrations)) => Checks {
            database: "ok",
            migrations,
        },
        _ => Checks {
            database: "unavailable",
            migrations: "unknown",
        },
    };
    if checks.migrations == "ok" {
        (
            StatusCode::OK,
            Json(Readiness {
                status: "ready",
                checks,
            }),
        )
    } else {
        let body = Readiness {
            status: "unavailable",
            checks,
        };
        (StatusCode::SERVICE_UNAVAILABLE, Json(body))
    }
}

/// This OpenAPI document.
#[utoipa::path(get, path = "/api/openapi.json", tag = "operations",
    responses((status = 200, description = "OpenAPI 3.1 document", content_type = "application/json")))]
async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(openapi())
}

/// The API with development auth settings; `main` uses [`app_with`].
pub fn app(state: AppState) -> Router {
    app_with(state, auth::Settings::default())
}

pub fn app_with(state: AppState, settings: auth::Settings) -> Router {
    let routes = Router::new()
        .route("/api/health", get(health))
        .route("/api/ready", get(ready))
        .route("/api/openapi.json", get(openapi_json))
        .route("/api/setup", get(auth::setup_status).post(auth::setup))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route(
            "/api/auth/me",
            get(auth::me).patch(accounts::update_profile),
        )
        .route("/api/auth/password", post(auth::change_password))
        .route("/api/auth/recovery", post(accounts::request_recovery))
        .route("/api/auth/recovery/reset", post(accounts::reset_password))
        .route(
            "/api/invitations",
            get(accounts::list_invitations).post(accounts::create_invitation),
        )
        .route("/api/invitations/lookup", post(accounts::lookup_invitation))
        .route("/api/invitations/accept", post(accounts::accept_invitation))
        .route(
            "/api/invitations/{id}/revoke",
            post(accounts::revoke_invitation),
        )
        .route("/api/accounts", get(accounts::list_accounts))
        .route(
            "/api/accounts/{id}",
            axum::routing::put(management::update_account),
        )
        .route(
            "/api/teams",
            get(management::list_teams).post(management::create_team),
        )
        .route(
            "/api/teams/{id}",
            axum::routing::put(management::update_team),
        )
        .route("/api/organization", get(management::organization))
        .route(
            "/api/admin/settings",
            get(management::settings).put(management::update_settings),
        )
        .route(
            "/api/invitations/{id}/reissue",
            post(accounts::reissue_invitation),
        )
        .route(
            "/api/accounts/{id}/deactivate",
            post(accounts::deactivate_account),
        )
        .route(
            "/api/accounts/{id}/reactivate",
            post(accounts::reactivate_account),
        )
        .route(
            "/api/settings/smtp",
            get(smtp::get_smtp)
                .put(smtp::put_smtp)
                .delete(smtp::delete_smtp),
        )
        .route("/api/settings/smtp/test", post(smtp::test_smtp))
        .route("/api/catalog", get(catalog::active_catalog))
        .route("/api/catalog/all", get(catalog::full_catalog))
        .route("/api/categories", post(catalog::create_category))
        .route("/api/categories/order", put(catalog::reorder_categories))
        .route("/api/categories/{id}", patch(catalog::update_category))
        .route("/api/priorities", post(catalog::create_priority))
        .route("/api/priorities/order", put(catalog::reorder_priorities))
        .route("/api/priorities/{id}", patch(catalog::update_priority))
        .route("/api/intake-fields", post(catalog::create_intake_field))
        .route(
            "/api/intake-fields/order",
            put(catalog::reorder_intake_fields),
        )
        .route(
            "/api/intake-fields/{id}",
            patch(catalog::update_intake_field),
        )
        .route(
            "/api/tickets",
            get(tickets::list_own_tickets).post(submission::submit_ticket),
        )
        .route(
            "/api/ticket-drafts",
            get(submission::list_drafts).post(submission::create_draft),
        )
        .route(
            "/api/ticket-drafts/{id}",
            get(submission::get_draft)
                .patch(submission::update_draft)
                .delete(submission::delete_draft),
        )
        .route(
            "/api/ticket-drafts/{id}/submit",
            post(submission::submit_draft),
        )
        .route("/api/tickets/{id}", get(tickets::ticket_detail))
        .route("/api/tickets/{id}/actions", post(tickets::requester_action))
        .route(
            "/api/tickets/{id}/activity",
            get(tickets::ticket_activity)
                .layer(axum::middleware::map_response(api::private_response)),
        )
        .route(
            "/api/tickets/{id}/staff-actions",
            post(tickets::staff_action),
        )
        .route(
            "/api/tickets/{id}/assignment",
            post(assignment::change_assignment),
        )
        .route(
            "/api/tickets/{id}/messages",
            get(messages::list).layer(axum::middleware::map_response(api::private_response)),
        )
        .route(
            "/api/tickets/{id}/replies",
            post(messages::reply).layer(axum::middleware::map_response(api::private_response)),
        )
        .route(
            "/api/tickets/{id}/notes",
            post(messages::note).layer(axum::middleware::map_response(api::private_response)),
        )
        .route(
            "/api/screenshots",
            post(screenshots::upload).layer(DefaultBodyLimit::max(screenshots::MAX_BYTES)),
        )
        .route(
            "/api/screenshots/{id}",
            get(screenshots::metadata)
                .delete(screenshots::delete)
                .layer(axum::middleware::map_response(api::private_response)),
        )
        .route("/api/screenshots/{id}/preview", get(screenshots::preview))
        .route("/api/screenshots/{id}/download", get(screenshots::download))
        .route("/api/queue", get(queues::list_queue))
        .route("/api/queue/counts", get(queues::queue_counts))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::session,
        ))
        .layer(axum::middleware::from_fn(auth::csrf))
        .layer(Extension(Arc::new(auth::Auth::new(settings))))
        .with_state(state);
    with_conventions(routes)
}

/// Request IDs, tracing, panic safety, body limit, and the error envelope.
/// Layers run outermost-last: the request ID is set before anything else.
pub fn with_conventions(router: Router) -> Router {
    router
        .layer(DefaultBodyLimit::max(api::MAX_JSON_BODY))
        .layer(CatchPanicLayer::custom(|_| {
            tracing::error!("request handler panicked");
            ApiError::Internal.into_response()
        }))
        .layer(axum::middleware::from_fn(api::error_envelope))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            // Logs the path only: query strings may carry tokens.
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    let id = request.headers().get("x-request-id").and_then(|v| v.to_str().ok());
                    tracing::info_span!("request", method = %request.method(), path = request.uri().path(), request_id = id)
                })
                .on_response(DefaultOnResponse::new().level(tracing::Level::INFO)),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        response::Response,
        routing::{get, post},
    };
    use serde::Deserialize;
    use serde_json::{Value, json};
    use sqlx::postgres::PgPoolOptions;
    use time::OffsetDateTime;
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::api::{Cursor, Json, Page, Pagination, Path, Query};

    fn lazy_state() -> super::AppState {
        // Never connects unless a handler touches the database.
        let pool = PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(200))
            .connect_lazy("postgres://tikkit@127.0.0.1:1/tikkit")
            .unwrap();
        super::AppState {
            pool,
            screenshot_dir: std::env::temp_dir().into(),
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct NewThing {
        name: String,
        count: u8,
    }

    fn test_app() -> Router {
        let routes =
            Router::new()
                .route(
                    "/api/things/{id}",
                    get(|Path(id): Path<Uuid>| async move { id.to_string() }),
                )
                .route(
                    "/api/things",
                    post(|Json(_): Json<NewThing>| async { "created" }),
                )
                .route(
                    "/api/list",
                    get(|Query(p): Query<Pagination>| async move {
                        Json(json!({ "limit": p.limit() }))
                    }),
                )
                .route(
                    "/api/panic",
                    get(|| async { panic!("boom") as &'static str }),
                )
                .merge(super::app(lazy_state()));
        super::with_conventions(routes)
    }

    async fn send(request: Request<Body>) -> (Response, Value) {
        let response = test_app().oneshot(request).await.unwrap();
        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (Response::from_parts(parts, Body::empty()), value)
    }

    fn post_json(body: impl Into<Body>) -> Request<Body> {
        Request::post("/api/things")
            .header("content-type", "application/json")
            .body(body.into())
            .unwrap()
    }

    fn assert_error(response: &Response, body: &Value, status: StatusCode, code: &str) {
        assert_eq!(response.status(), status, "{body}");
        assert_eq!(body["error"]["code"], code, "{body}");
        assert!(body["error"]["message"].is_string());
        let header = response.headers()["x-request-id"].to_str().unwrap();
        assert_eq!(body["request_id"], header);
    }

    #[tokio::test]
    async fn health_returns_json_status() {
        let response = super::app(lazy_state())
            .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "application/json");
        assert_eq!(response.headers()["cache-control"], "private, no-store");
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload, serde_json::json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn ready_is_unavailable_without_database() {
        let (response, body) = send(Request::get("/api/ready").body(Body::empty()).unwrap()).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let expected = json!({
            "status": "unavailable",
            "checks": { "database": "unavailable", "migrations": "unknown" }
        });
        assert_eq!(body, expected);
    }

    #[tokio::test]
    async fn request_ids_are_generated_or_propagated() {
        let (response, _) = send(Request::get("/api/health").body(Body::empty()).unwrap()).await;
        let generated = response.headers()["x-request-id"].to_str().unwrap();
        assert!(Uuid::parse_str(generated).is_ok());

        let request = Request::get("/api/nope").header("x-request-id", "abc-123");
        let (response, body) = send(request.body(Body::empty()).unwrap()).await;
        assert_eq!(response.headers()["x-request-id"], "abc-123");
        assert_eq!(body["request_id"], "abc-123");
    }

    #[tokio::test]
    async fn unknown_routes_and_methods_use_the_envelope() {
        let (response, body) = send(Request::get("/api/nope").body(Body::empty()).unwrap()).await;
        assert_error(&response, &body, StatusCode::NOT_FOUND, "not_found");
        assert_eq!(response.headers()["content-type"], "application/json");
        let request = Request::delete("/api/health").header("x-requested-with", "tikkit");
        let (response, body) = send(request.body(Body::empty()).unwrap()).await;
        assert_error(
            &response,
            &body,
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        );
        assert_eq!(response.headers()["allow"], "GET,HEAD");
    }

    #[tokio::test]
    async fn json_bodies_are_validated() {
        let (r, b) = send(post_json(r#"{"name": "x", "count": 1}"#)).await;
        assert_eq!(r.status(), StatusCode::OK, "{b}");

        let (r, b) = send(post_json("{not json")).await;
        assert_error(&r, &b, StatusCode::BAD_REQUEST, "invalid_json");

        let (r, b) = send(post_json(r#"{"name": "x", "count": 1, "admin": true}"#)).await;
        assert_error(&r, &b, StatusCode::UNPROCESSABLE_ENTITY, "unknown_field");
        assert_eq!(
            b["error"]["details"],
            json!({ "fields": { "admin": ["unknown_field"] } })
        );

        let (r, b) = send(post_json(r#"{"count": 1}"#)).await;
        assert_error(
            &r,
            &b,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
        );
        assert_eq!(
            b["error"]["details"],
            json!({ "fields": { "name": ["required"] } })
        );

        let (r, b) = send(post_json(r#"{"name": "x", "count": 999}"#)).await;
        assert_eq!(
            b["error"]["details"],
            json!({ "fields": { "count": ["invalid"] } }),
            "{r:?}"
        );

        let request = Request::post("/api/things").body(Body::from("{}")).unwrap();
        let (r, b) = send(request).await;
        assert_error(
            &r,
            &b,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
        );

        let huge = format!(
            r#"{{"name": "{}", "count": 1}}"#,
            "x".repeat(super::api::MAX_JSON_BODY)
        );
        let (r, b) = send(post_json(huge)).await;
        assert_error(&r, &b, StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large");
    }

    #[tokio::test]
    async fn malformed_identifiers_and_queries_are_rejected() {
        let id = Uuid::new_v4();
        let (r, _) = send(
            Request::get(format!("/api/things/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        let (r, b) = send(
            Request::get("/api/things/not-a-uuid")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_error(&r, &b, StatusCode::BAD_REQUEST, "invalid_identifier");
        let (r, b) = send(
            Request::get("/api/list?limit=ten")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_error(&r, &b, StatusCode::BAD_REQUEST, "invalid_query");
    }

    #[tokio::test]
    async fn panics_become_internal_errors() {
        let (r, b) = send(Request::get("/api/panic").body(Body::empty()).unwrap()).await;
        assert_error(&r, &b, StatusCode::INTERNAL_SERVER_ERROR, "internal");
    }

    #[tokio::test]
    async fn pagination_limits_are_clamped() {
        for (query, limit) in [
            ("", 25),
            ("?limit=0", 1),
            ("?limit=-5", 1),
            ("?limit=40", 40),
            ("?limit=1000", 100),
        ] {
            let (_, b) = send(
                Request::get(format!("/api/list{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(b["limit"], limit, "{query}");
        }
    }

    #[test]
    fn cursors_round_trip_and_pages_know_when_to_stop() {
        let cursor = Cursor {
            created_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            id: Uuid::new_v4(),
        };
        let page = Page::from_rows(vec![cursor; 3], 2, |c| *c);
        assert_eq!(page.items.len(), 2);
        let next = Pagination {
            limit: None,
            cursor: page.next_cursor,
        };
        assert_eq!(next.cursor().unwrap(), Some(cursor));
        assert!(
            Page::from_rows(vec![cursor; 2], 2, |c| *c)
                .next_cursor
                .is_none()
        );
        let bad = Pagination {
            limit: None,
            cursor: Some("bm9wZQ".into()),
        };
        assert!(bad.cursor().is_err());
    }

    #[tokio::test]
    async fn served_openapi_matches_committed_file() {
        let (r, served) = send(
            Request::get("/api/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        let committed = include_str!("../openapi.json");
        assert_eq!(
            super::openapi().to_pretty_json().unwrap() + "\n",
            committed,
            "apps/api/openapi.json is stale; run: cargo run --locked -p tikkit-api --bin openapi > apps/api/openapi.json"
        );
        assert_eq!(served, serde_json::from_str::<Value>(committed).unwrap());
    }
}

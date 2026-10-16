//! OpenAPI document for the API (utoipa).
//!
//! The committed `apps/api/openapi.json` must match [`spec_json`]; the test
//! `openapi_spec_is_up_to_date` fails on drift. Regenerate with
//! `UPDATE_OPENAPI=1 cargo test --locked -p tikkit-api openapi`, then
//! `bun run --cwd apps/web generate:api` for the typed web client.
//!
//! Every endpoint gets `#[utoipa::path]` and is listed in `paths(...)`; every
//! DTO it uses must derive `ToSchema` (it is picked up automatically when
//! referenced, or list it in `components(schemas(...))`).

use axum::{
    Json,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use utoipa::{
    Modify, OpenApi,
    openapi::security::{ApiKey, ApiKeyValue, SecurityScheme},
};

use crate::{
    auth::{
        account::{Account, AccountTeam, SessionInfo},
        handlers::{LoginRequest, SetupRequest, SetupStatus},
    },
    domain::{AccountStatus, Role},
    dto::{AssignmentQueue, Language, PublicAssignee, PublicTeam, PublicTicketAssignment},
    error::{ErrorBody, ErrorCode, ErrorDetail, FieldError, FieldErrorCode},
};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Tikkit API",
        license(name = "Apache-2.0", identifier = "Apache-2.0"),
        description = "Self-hosted IT ticketing API. Product endpoints live under `/api/v1`. \
            Every non-2xx response uses the `ErrorBody` shape; see docs/api.md."
    ),
    servers((url = "/", description = "Same origin as the web app")),
    paths(
        crate::health::live,
        crate::health::live_alias,
        crate::health::ready,
        serve,
        crate::auth::handlers::setup_status,
        crate::auth::handlers::setup,
        crate::auth::handlers::login,
        crate::auth::handlers::logout,
        crate::auth::handlers::current_session,
    ),
    modifiers(&SessionCookieScheme),
    components(schemas(
        Role,
        AccountStatus,
        Account,
        AccountTeam,
        SessionInfo,
        SetupStatus,
        SetupRequest,
        LoginRequest,
        ErrorBody,
        ErrorDetail,
        ErrorCode,
        FieldError,
        FieldErrorCode,
        Language,
        AssignmentQueue,
        PublicTeam,
        PublicAssignee,
        PublicTicketAssignment,
    )),
    tags(
        (name = "system", description = "Health and API metadata"),
        (name = "setup", description = "One-time first setup (initial Admin)"),
        (name = "auth", description = "Login, logout and the current session. Unsafe requests need `Origin: <PUBLIC_ORIGIN>`."),
    )
)]
pub struct ApiDoc;

/// Documents the session cookie (`__Host-tikkit_session` in production).
struct SessionCookieScheme;

impl Modify for SessionCookieScheme {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "session_cookie",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                crate::auth::cookie::DEVELOPMENT_COOKIE_NAME,
                "HttpOnly session cookie set by setup/login. Named `__Host-tikkit_session` in production.",
            ))),
        );
    }
}

/// Pretty-printed spec as committed in `apps/api/openapi.json`.
pub fn spec_json() -> String {
    let mut json = ApiDoc::openapi()
        .to_pretty_json()
        .expect("OpenAPI document serializes");
    json.push('\n');
    json
}

/// The OpenAPI document of this API.
#[utoipa::path(
    get,
    path = "/api/openapi.json",
    tag = "system",
    operation_id = "getOpenApi",
    responses((status = 200, description = "OpenAPI 3.1 document", content_type = "application/json", body = Object))
)]
pub async fn serve() -> Response {
    let mut response = Json(ApiDoc::openapi()).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_spec_is_up_to_date() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/openapi.json");
        let generated = spec_json();
        if std::env::var_os("UPDATE_OPENAPI").is_some_and(|v| v == "1") {
            std::fs::write(path, &generated).expect("write openapi.json");
            return;
        }
        let committed = std::fs::read_to_string(path).unwrap_or_default();
        // `info.version` follows the crate version, which release tooling
        // bumps; ignore it so a release bump alone is not reported as drift.
        let normalize = |s: &str| -> serde_json::Value {
            let mut value: serde_json::Value = serde_json::from_str(s).unwrap_or_default();
            if let Some(info) = value.get_mut("info") {
                info["version"] = serde_json::Value::Null;
            }
            value
        };
        assert!(
            normalize(&committed) == normalize(&generated),
            "apps/api/openapi.json is out of date. Run \
             `UPDATE_OPENAPI=1 cargo test --locked -p tikkit-api openapi` and \
             `bun run --cwd apps/web generate:api`, then commit the results."
        );
    }

    #[test]
    fn spec_has_contract_basics() {
        let spec: serde_json::Value = serde_json::from_str(&spec_json()).unwrap();
        assert_eq!(spec["info"]["title"], "Tikkit API");
        assert_eq!(spec["info"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(spec["servers"][0]["url"], "/");
        let codes = &spec["components"]["schemas"]["ErrorCode"]["enum"];
        assert!(
            codes
                .as_array()
                .unwrap()
                .contains(&"validation_failed".into())
        );
    }
}

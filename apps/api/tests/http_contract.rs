//! HTTP contract tests: drives a router built from the public contract
//! modules with test-only routes (no fake endpoints ship in the crate).

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::Response,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tikkit_api::{
    error::{ApiError, FieldErrorCode},
    extract::{ApiJson, ApiPath, ApiQuery, FieldErrors, ValidJson, Validate},
    http::apply_api_layers,
    pagination::{Page, PageParams},
};
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CreateThing {
    display_name: String,
    count: u32,
}

impl Validate for CreateThing {
    fn validate(&self) -> Result<(), ApiError> {
        let mut errors = FieldErrors::new();
        errors
            .length("displayName", self.display_name.trim(), 3, 32)
            .range("count", i64::from(self.count), 1, 10);
        errors.finish()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Thing {
    id: Uuid,
    display_name: String,
}

async fn create_thing(ValidJson(body): ValidJson<CreateThing>) -> (StatusCode, ApiJson<Thing>) {
    let thing = Thing {
        id: Uuid::nil(),
        display_name: body.display_name,
    };
    (StatusCode::CREATED, ApiJson(thing))
}

async fn get_thing(ApiPath(id): ApiPath<Uuid>) -> Result<ApiJson<Value>, ApiError> {
    if id.is_nil() {
        Ok(ApiJson(json!({ "id": id })))
    } else {
        Err(ApiError::not_found())
    }
}

async fn list_things(
    ApiQuery(params): ApiQuery<PageParams>,
) -> Result<ApiJson<Page<u32>>, ApiError> {
    let request = params.validate::<u32>()?;
    let start = request.after.map_or(1, |last| last + 1);
    let rows: Vec<u32> = (start..=5).take(request.fetch_limit() as usize).collect();
    Ok(ApiJson(Page::from_overfetched(rows, &request, |n| *n)))
}

async fn fail() -> Result<(), ApiError> {
    Err(ApiError::internal(
        "connection to 10.0.0.5 refused; secret=abc",
    ))
}

async fn panic_handler() -> &'static str {
    panic!("boom with secret=abc")
}

fn router() -> Router {
    let router = Router::new()
        .route("/api/v1/things", post(create_thing).get(list_things))
        .route("/api/v1/things/{id}", get(get_thing))
        .route("/api/v1/fail", get(fail))
        .route("/api/v1/panic", get(panic_handler));
    apply_api_layers(router)
}

async fn send(request: Request<Body>) -> (StatusCode, Response) {
    let response = router().oneshot(request).await.unwrap();
    (response.status(), response)
}

async fn json_body(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn post_json(uri: &str, body: impl Into<Body>) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.into())
        .unwrap()
}

/// Asserts the contract error shape and returns the `error` object.
async fn expect_error(response: Response, status: StatusCode, code: &str) -> Value {
    assert_eq!(response.status(), status);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let header_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let body = json_body(response).await;
    let error = body["error"].clone();
    assert_eq!(error["code"], code, "body: {body}");
    assert!(error["message"].as_str().is_some_and(|m| !m.is_empty()));
    assert_eq!(error["requestId"], header_id.as_str());
    assert_eq!(
        body.as_object().unwrap().len(),
        1,
        "only `error` at top level"
    );
    error
}

#[tokio::test]
async fn success_with_valid_body() {
    let (status, response) = send(post_json(
        "/api/v1/things",
        r#"{"displayName":"Printer","count":2}"#,
    ))
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(response.headers().contains_key("x-request-id"));
    assert_eq!(
        json_body(response).await,
        json!({"id": Uuid::nil(), "displayName": "Printer"})
    );
}

#[tokio::test]
async fn validation_failed_lists_fields() {
    let (_, response) = send(post_json(
        "/api/v1/things",
        r#"{"displayName":"ab","count":0}"#,
    ))
    .await;
    let error = expect_error(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    )
    .await;
    assert_eq!(
        error["fields"],
        json!([
            {"field": "displayName", "code": "too_short", "params": {"min": 3}},
            {"field": "count", "code": "out_of_range", "params": {"min": 1, "max": 10}},
        ])
    );
}

#[tokio::test]
async fn malformed_json_is_invalid_request() {
    let (_, response) = send(post_json("/api/v1/things", r#"{"displayName":"#)).await;
    let error = expect_error(response, StatusCode::BAD_REQUEST, "invalid_request").await;
    assert!(error.get("fields").is_none());
}

#[tokio::test]
async fn unknown_and_mistyped_fields_are_invalid_request() {
    let (_, response) = send(post_json(
        "/api/v1/things",
        r#"{"displayName":"Printer","count":2,"isAdmin":true}"#,
    ))
    .await;
    let error = expect_error(response, StatusCode::BAD_REQUEST, "invalid_request").await;
    assert_eq!(
        error["fields"],
        json!([{"field": "isAdmin", "code": "unknown_field"}])
    );
    // The raw serde message (which lists expected fields) is not exposed.
    assert!(!error["message"].as_str().unwrap().contains("expected"));

    let (_, response) = send(post_json(
        "/api/v1/things",
        r#"{"displayName":"P","count":"x"}"#,
    ))
    .await;
    let error = expect_error(response, StatusCode::BAD_REQUEST, "invalid_request").await;
    assert_eq!(
        error["fields"],
        json!([{"field": "count", "code": "invalid_type"}])
    );

    let (_, response) = send(post_json("/api/v1/things", r#"{"count":2}"#)).await;
    let error = expect_error(response, StatusCode::BAD_REQUEST, "invalid_request").await;
    assert_eq!(
        error["fields"],
        json!([{"field": "displayName", "code": "required"}])
    );
}

#[tokio::test]
async fn wrong_or_missing_content_type_is_invalid_request() {
    for content_type in [
        Some("text/plain"),
        Some("application/x-www-form-urlencoded"),
        None,
    ] {
        let mut request = Request::post("/api/v1/things");
        if let Some(content_type) = content_type {
            request = request.header(header::CONTENT_TYPE, content_type);
        }
        let body = Body::from(r#"{"displayName":"Printer","count":2}"#);
        let (_, response) = send(request.body(body).unwrap()).await;
        expect_error(response, StatusCode::BAD_REQUEST, "invalid_request").await;
    }
    let (status, _) = send(
        Request::post("/api/v1/things")
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            .body(Body::from(r#"{"displayName":"Printer","count":2}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn oversized_body_is_payload_too_large() {
    let name = "a".repeat(70 * 1024);
    let body = format!(r#"{{"displayName":"{name}","count":2}}"#);
    let (_, response) = send(post_json("/api/v1/things", body)).await;
    expect_error(response, StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large").await;
}

#[tokio::test]
async fn unknown_route_is_not_found() {
    let (_, response) = send(Request::get("/api/v1/nope").body(Body::empty()).unwrap()).await;
    expect_error(response, StatusCode::NOT_FOUND, "not_found").await;
}

#[tokio::test]
async fn invalid_path_id_is_not_found() {
    let (_, response) = send(
        Request::get("/api/v1/things/not-a-uuid")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let invalid = expect_error(response, StatusCode::NOT_FOUND, "not_found").await;

    let uri = format!("/api/v1/things/{}", Uuid::now_v7());
    let (_, response) = send(Request::get(uri).body(Body::empty()).unwrap()).await;
    let missing = expect_error(response, StatusCode::NOT_FOUND, "not_found").await;
    // Indistinguishable apart from the request id.
    assert_eq!(invalid["message"], missing["message"]);

    let uri = format!("/api/v1/things/{}", Uuid::nil());
    let (status, _) = send(Request::get(uri).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn wrong_method_is_method_not_allowed() {
    let (_, response) = send(
        Request::delete("/api/v1/things")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.headers()[header::ALLOW], "POST,GET,HEAD");
    expect_error(
        response,
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
    )
    .await;
}

#[tokio::test]
async fn internal_errors_hide_details() {
    for uri in ["/api/v1/fail", "/api/v1/panic"] {
        let (_, response) = send(Request::get(uri).body(Body::empty()).unwrap()).await;
        let error = expect_error(
            response,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
        )
        .await;
        assert_eq!(error["message"], "An unexpected error occurred.");
        assert!(!error.to_string().contains("secret"));
    }
}

#[tokio::test]
async fn request_id_is_echoed_when_sane() {
    let (_, response) = send(
        Request::get("/api/v1/nope")
            .header("x-request-id", "proxy-abc.123_X")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.headers()["x-request-id"], "proxy-abc.123_X");
    let error = expect_error(response, StatusCode::NOT_FOUND, "not_found").await;
    assert_eq!(error["requestId"], "proxy-abc.123_X");
}

#[tokio::test]
async fn request_id_is_replaced_when_unsafe_or_missing() {
    let long = "a".repeat(65);
    for inbound in [
        None,
        Some("has space"),
        Some("<script>"),
        Some(long.as_str()),
    ] {
        let mut request = Request::get("/api/v1/nope");
        if let Some(inbound) = inbound {
            request = request.header("x-request-id", inbound);
        }
        let (_, response) = send(request.body(Body::empty()).unwrap()).await;
        let id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        assert!(Uuid::parse_str(&id).is_ok(), "generated id {id}");
        assert_ne!(Some(id.as_str()), inbound);
        let error = expect_error(response, StatusCode::NOT_FOUND, "not_found").await;
        assert_eq!(error["requestId"], id.as_str());
    }
}

#[tokio::test]
async fn pagination_walks_pages() {
    let (status, response) = send(
        Request::get("/api/v1/things?limit=2")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let first = json_body(response).await;
    assert_eq!(first["items"], json!([1, 2]));
    let cursor = first["nextCursor"].as_str().unwrap().to_owned();

    let uri = format!("/api/v1/things?limit=2&cursor={cursor}");
    let second = json_body(send(Request::get(uri).body(Body::empty()).unwrap()).await.1).await;
    assert_eq!(second["items"], json!([3, 4]));

    let uri = format!(
        "/api/v1/things?limit=2&cursor={}",
        second["nextCursor"].as_str().unwrap()
    );
    let last = json_body(send(Request::get(uri).body(Body::empty()).unwrap()).await.1).await;
    assert_eq!(last, json!({"items": [5], "nextCursor": null}));

    let all = json_body(
        send(Request::get("/api/v1/things").body(Body::empty()).unwrap())
            .await
            .1,
    )
    .await;
    assert_eq!(all["items"], json!([1, 2, 3, 4, 5]));
}

#[tokio::test]
async fn pagination_rejects_bad_limits_cursors_and_unknown_params() {
    for (query, field) in [
        ("limit=0", "limit"),
        ("limit=101", "limit"),
        ("limit=ten", "limit"),
        ("cursor=bm90LWpzb24", "cursor"),
        ("cursor=%22%7B%7D", "cursor"),
    ] {
        let uri = format!("/api/v1/things?{query}");
        let (_, response) = send(Request::get(uri).body(Body::empty()).unwrap()).await;
        let error = expect_error(
            response,
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
        )
        .await;
        assert_eq!(error["fields"][0]["field"], field, "{query}");
    }

    let (_, response) = send(
        Request::get("/api/v1/things?limit=2&sort=evil")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let error = expect_error(response, StatusCode::BAD_REQUEST, "invalid_request").await;
    assert_eq!(
        error["fields"],
        json!([{"field": "sort", "code": "unknown_field"}])
    );
}

#[test]
fn field_error_codes_are_stable_strings() {
    assert_eq!(
        serde_json::to_value(FieldErrorCode::TooShort).unwrap(),
        json!("too_short")
    );
}

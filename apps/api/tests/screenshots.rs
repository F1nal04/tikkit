//! Screenshot uploads, storage, and access against real PostgreSQL and a temp volume.
use std::{io::Cursor, net::SocketAddr};

use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode},
};
use image::{DynamicImage, ImageFormat, RgbaImage};
use serde_json::{Value, json};
use tikkit_api::{AppState, screenshots};
use tower::ServiceExt;
use uuid::Uuid;

mod support;
use support::{
    TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, screenshot_dir, test_app,
};

struct World {
    db: TestDb,
    app: Router,
    requester: String,
    category: Uuid,
    priority: Uuid,
}

async fn world() -> Option<World> {
    let db = TestDb::new().await?;
    fixtures::bootstrap_admin(&db.pool).await;
    fixtures::account(&db.pool, Role::User, "requester").await;
    let category =
        sqlx::query_scalar("INSERT INTO categories (name) VALUES ('Software') RETURNING id")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let priority =
        sqlx::query_scalar("INSERT INTO priorities (name) VALUES ('Normal') RETURNING id")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let app = test_app(&db.pool);
    let requester = login(&app, "requester", PASSWORD).await.session();
    Some(World {
        db,
        app,
        requester,
        category,
        priority,
    })
}

struct Raw {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Raw {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn header(&self, name: &str) -> &str {
        self.headers.get(name).map_or("", |v| v.to_str().unwrap())
    }
    /// The first field error code of a `validation_failed` reply.
    fn field(&self, name: &str) -> String {
        self.json()["error"]["details"]["fields"][name][0]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
}

async fn raw(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: &str,
    upload: Option<(&str, Vec<u8>)>,
) -> Raw {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-requested-with", "tikkit")
        .header("cookie", cookie);
    let body = match upload {
        Some((content_type, bytes)) => {
            request = request.header("content-type", content_type);
            Body::from(bytes)
        }
        None => Body::empty(),
    };
    let mut request = request.body(body).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))));
    let response = app.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    Raw {
        status: parts.status,
        headers: parts.headers,
        body: to_bytes(body, usize::MAX).await.unwrap(),
    }
}

async fn upload(
    app: &Router,
    cookie: &str,
    filename: &str,
    content_type: &str,
    bytes: Vec<u8>,
) -> Raw {
    let uri = format!("/api/screenshots?filename={}", urlencode(filename));
    raw(app, "POST", &uri, cookie, Some((content_type, bytes))).await
}

fn urlencode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn encoded(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let image = RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([x as u8, y as u8, 128, 255])
    });
    let image = if format == ImageFormat::Jpeg {
        DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(image).to_rgb8())
    } else {
        DynamicImage::ImageRgba8(image)
    };
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, format).unwrap();
    out.into_inner()
}

fn png() -> Vec<u8> {
    encoded(ImageFormat::Png, 4, 3)
}

async fn upload_png(w: &World, cookie: &str) -> Uuid {
    let r = upload(&w.app, cookie, "shot.png", "image/png", png()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    r.json()["id"].as_str().unwrap().parse().unwrap()
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(kind);
    chunk.extend_from_slice(data);
    let mut crc = kind.to_vec();
    crc.extend_from_slice(data);
    chunk.extend_from_slice(&crc32(&crc).to_be_bytes());
    chunk
}

/// A valid PNG with `chunk` inserted right after IHDR (8-byte signature + 25-byte IHDR).
fn png_with(chunk: Vec<u8>) -> Vec<u8> {
    let mut bytes = png();
    bytes.splice(33..33, chunk);
    bytes
}

/// A PNG whose header claims `width × height` but carries a tiny payload.
fn png_claiming(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = png();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&bytes[24..29]);
    bytes.splice(8..33, png_chunk(b"IHDR", &ihdr));
    bytes
}

fn riff_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut chunk = kind.to_vec();
    chunk.extend_from_slice(&(data.len() as u32).to_le_bytes());
    chunk.extend_from_slice(data);
    if data.len() % 2 == 1 {
        chunk.push(0);
    }
    chunk
}

/// A one-frame animated WebP built around a real lossless frame.
fn animated_webp() -> Vec<u8> {
    let frame = encoded(ImageFormat::WebP, 1, 1)[12..].to_vec(); // its VP8L chunk
    let mut vp8x = vec![0x02, 0, 0, 0]; // animation flag
    vp8x.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // 1×1 canvas (minus one)
    let mut anmf = vec![0u8; 12]; // x, y, width-1, height-1
    anmf.extend_from_slice(&[100, 0, 0, 0]); // duration, flags
    anmf.extend_from_slice(&frame);
    let mut body = b"WEBP".to_vec();
    body.extend(riff_chunk(b"VP8X", &vp8x));
    body.extend(riff_chunk(b"ANIM", &[0, 0, 0, 0, 0, 0]));
    body.extend(riff_chunk(b"ANMF", &anmf));
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend(body);
    file
}

async fn count(w: &World) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM screenshots")
        .fetch_one(&w.db.pool)
        .await
        .unwrap()
}

fn stored_files(w: &World) -> Vec<String> {
    std::fs::read_dir(screenshot_dir(&w.db.pool))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn each_supported_format_is_decoded_reencoded_and_served_safely() {
    let Some(w) = world().await else { return };
    let cases = [
        (
            ImageFormat::Png,
            "image/png",
            "Bildschirmfoto Übersicht.PNG",
            "Bildschirmfoto Übersicht.png",
        ),
        (ImageFormat::Jpeg, "image/jpeg", "photo.jpeg", "photo.jpg"),
        (
            ImageFormat::WebP,
            "image/webp",
            "../../etc/passwd",
            "passwd.webp",
        ),
    ];
    for (format, mime, name, normalized) in cases {
        let r = upload(&w.app, &w.requester, name, mime, encoded(format, 40, 30)).await;
        assert_eq!(r.status, StatusCode::CREATED, "{mime}: {:?}", r.json());
        let meta = r.json();
        assert_eq!(meta["content_type"], mime);
        assert_eq!(meta["filename"], normalized);
        assert_eq!(
            (meta["width"].as_i64(), meta["height"].as_i64()),
            (Some(40), Some(30))
        );
        let id = meta["id"].as_str().unwrap();

        let r = raw(
            &w.app,
            "GET",
            &format!("/api/screenshots/{id}"),
            &w.requester,
            None,
        )
        .await;
        assert_eq!(r.json(), meta);
        for (endpoint, disposition) in [("preview", "inline"), ("download", "attachment")] {
            let r = raw(
                &w.app,
                "GET",
                &format!("/api/screenshots/{id}/{endpoint}"),
                &w.requester,
                None,
            )
            .await;
            assert_eq!(r.status, StatusCode::OK);
            assert_eq!(r.header("content-type"), mime);
            assert_eq!(r.header("x-content-type-options"), "nosniff");
            assert_eq!(r.header("cache-control"), "private, no-store");
            assert!(r.header("content-security-policy").contains("sandbox"));
            assert!(
                r.header("content-disposition").starts_with(disposition),
                "{}",
                r.header("content-disposition")
            );
            assert!(
                r.header("content-disposition")
                    .contains("filename*=UTF-8''")
            );
            assert_eq!(r.body.len() as i64, meta["byte_size"].as_i64().unwrap());
            let decoded = image::load_from_memory_with_format(&r.body, format).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (40, 30));
        }
    }
    assert_eq!(stored_files(&w).len(), 3, "named by ID only");

    // Metadata is stripped by re-encoding: text chunks and EXIF never reach storage.
    let tagged = png_with(png_chunk(b"tEXt", b"Comment\0<script>alert(1)</script>"));
    let r = upload(&w.app, &w.requester, "x.png", "image/png", tagged).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let stored = std::fs::read(screenshot_dir(&w.db.pool).join(&id)).unwrap();
    assert!(!stored.windows(8).any(|s| s == b"<script>"));
    let mut jpeg = encoded(ImageFormat::Jpeg, 8, 8);
    let exif = b"Exif\0\0GPS-SECRET-LOCATION";
    let mut app1 = vec![0xFF, 0xE1];
    app1.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    app1.extend_from_slice(exif);
    jpeg.splice(2..2, app1);
    let r = upload(&w.app, &w.requester, "x.jpg", "image/jpeg", jpeg).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let stored = std::fs::read(screenshot_dir(&w.db.pool).join(&id)).unwrap();
    assert!(!stored.windows(10).any(|s| s == b"GPS-SECRET"));
}

#[tokio::test]
async fn spoofed_corrupt_animated_oversized_and_disallowed_files_are_rejected() {
    let Some(w) = world().await else { return };
    let mut truncated = png();
    truncated.truncate(40);
    let mut garbage_jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
    garbage_jpeg.extend_from_slice(&[0x42; 64]);
    let mut apng_control = 1u32.to_be_bytes().to_vec();
    apng_control.extend_from_slice(&0u32.to_be_bytes());
    let cases: Vec<(&str, &str, Vec<u8>, &str, &str)> = vec![
        (
            "x.svg",
            "image/svg+xml",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec(),
            "file",
            "unsupported_type",
        ),
        (
            "x.png",
            "image/png",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec(),
            "file",
            "unsupported_type",
        ),
        (
            "x.png",
            "image/png",
            b"<!doctype html><script>alert(1)</script>".to_vec(),
            "file",
            "unsupported_type",
        ),
        (
            "x.png",
            "image/png",
            b"%PDF-1.7\n".to_vec(),
            "file",
            "unsupported_type",
        ),
        (
            "x.png",
            "image/png",
            b"PK\x03\x04archive".to_vec(),
            "file",
            "unsupported_type",
        ),
        (
            "x.png",
            "image/png",
            b"MZ\x90\x00executable".to_vec(),
            "file",
            "unsupported_type",
        ),
        (
            "x.gif",
            "image/gif",
            b"GIF89a\x01\x00\x01\x00".to_vec(),
            "file",
            "unsupported_type",
        ),
        (
            "x.png",
            "application/octet-stream",
            png(),
            "file",
            "unsupported_type",
        ),
        ("x.png", "image/png", Vec::new(), "file", "required"),
        (
            "x.png",
            "image/png",
            encoded(ImageFormat::Jpeg, 4, 4),
            "file",
            "type_mismatch",
        ),
        ("x.jpg", "image/png", png(), "filename", "type_mismatch"),
        ("x.png.exe", "image/png", png(), "filename", "type_mismatch"),
        ("x.png", "image/png", truncated, "file", "corrupt"),
        ("x.jpg", "image/jpeg", garbage_jpeg, "file", "corrupt"),
        (
            "x.png",
            "image/png",
            png_with(png_chunk(b"acTL", &apng_control)),
            "file",
            "animated",
        ),
        ("x.webp", "image/webp", animated_webp(), "file", "animated"),
        (
            "x.png",
            "image/png",
            png_claiming(50_000, 50_000),
            "file",
            "dimensions_too_large",
        ),
        (
            "x.png",
            "image/png",
            png_claiming(7_000, 7_000),
            "file",
            "dimensions_too_large",
        ),
        (
            "x.png",
            "image/png",
            encoded(ImageFormat::Png, 16_385, 1),
            "file",
            "dimensions_too_large",
        ),
    ];
    for (name, mime, bytes, field, code) in cases {
        let r = upload(&w.app, &w.requester, name, mime, bytes).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{name} {mime}: {:?}",
            r.json()
        );
        assert_eq!(r.field(field), code, "{name} {mime}: {:?}", r.json());
    }
    // The route's own body limit replaces the 1 MiB JSON default.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let noise = RgbaImage::from_fn(1000, 800, |_, _| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        image::Rgba(state.to_le_bytes()[..4].try_into().unwrap())
    });
    let mut big = Cursor::new(Vec::new());
    noise.write_to(&mut big, ImageFormat::Png).unwrap();
    let big = big.into_inner();
    assert!(big.len() > 2 * 1024 * 1024);
    let r = upload(&w.app, &w.requester, "big.png", "image/png", big).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    sqlx::query("DELETE FROM screenshots")
        .execute(&w.db.pool)
        .await
        .unwrap();
    std::fs::remove_dir_all(screenshot_dir(&w.db.pool)).unwrap();

    let r = upload(
        &w.app,
        &w.requester,
        "x.png",
        "image/png",
        vec![0; screenshots::MAX_BYTES + 1],
    )
    .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(r.json()["error"]["code"], "payload_too_large");
    assert_eq!(count(&w).await, 0, "nothing recorded");
    assert!(stored_files(&w).is_empty(), "nothing stored");

    let r = raw(
        &w.app,
        "POST",
        "/api/screenshots",
        "",
        Some(("image/png", png())),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pending_uploads_are_capped_per_account() {
    let Some(w) = world().await else { return };
    let owner: Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE username = 'requester'")
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    // Cheaper than 50 real uploads; the cap only counts rows.
    sqlx::query(
        "INSERT INTO screenshots (id, uploader_account_id, filename, content_type, byte_size, width, height)
         SELECT gen_random_uuid(), $1, 'a.png', 'image/png', 1, 1, 1 FROM generate_series(1, $2)",
    )
    .bind(owner)
    .bind(screenshots::MAX_PENDING as i32)
    .execute(&w.db.pool)
    .await
    .unwrap();
    let r = upload(&w.app, &w.requester, "x.png", "image/png", png()).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.field("file"), "too_many");
}

#[tokio::test]
async fn uploads_stay_private_until_submission_freezes_them() {
    let Some(w) = world().await else { return };
    fixtures::account(&w.db.pool, Role::User, "other").await;
    let other = login(&w.app, "other", PASSWORD).await.session();
    let admin = login(&w.app, "admin", PASSWORD).await.session();
    let id = upload_png(&w, &w.requester).await;
    let paths = [
        format!("/api/screenshots/{id}"),
        format!("/api/screenshots/{id}/preview"),
        format!("/api/screenshots/{id}/download"),
    ];
    for cookie in [&other, &admin] {
        for path in &paths {
            let r = raw(&w.app, "GET", path, cookie, None).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
        }
        let r = raw(&w.app, "DELETE", &paths[0], cookie, None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    }
    let intake = |ids: Vec<Uuid>| {
        json!({"category_id": w.category, "priority_id": w.priority, "subject": "Crash",
            "description": "See screenshot", "screenshot_ids": ids})
    };
    // Another account cannot reference it, by draft or submission.
    let r = call(
        &w.app,
        "POST",
        "/api/ticket-drafts",
        Some(intake(vec![id])),
        Some(&other),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        r.body["error"]["details"]["fields"]["screenshot_ids"][0],
        "invalid"
    );
    let body = json!({"idempotency_key": Uuid::new_v4(), "intake": intake(vec![id])});
    let r = call(&w.app, "POST", "/api/tickets", Some(body), Some(&other)).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Limits, duplicates, unknown IDs.
    let r = call(
        &w.app,
        "POST",
        "/api/ticket-drafts",
        Some(intake(vec![id, id])),
        Some(&w.requester),
    )
    .await;
    assert_eq!(
        r.body["error"]["details"]["fields"]["screenshot_ids"][0],
        "invalid"
    );
    let r = call(
        &w.app,
        "POST",
        "/api/ticket-drafts",
        Some(intake(vec![Uuid::new_v4()])),
        Some(&w.requester),
    )
    .await;
    assert_eq!(
        r.body["error"]["details"]["fields"]["screenshot_ids"][0],
        "invalid"
    );
    let many = (0..=screenshots::MAX_PER_TICKET)
        .map(|_| Uuid::new_v4())
        .collect();
    let r = call(
        &w.app,
        "POST",
        "/api/ticket-drafts",
        Some(intake(many)),
        Some(&w.requester),
    )
    .await;
    assert_eq!(
        r.body["error"]["details"]["fields"]["screenshot_ids"][0],
        "too_many"
    );

    // The owner's draft holds it; it cannot be deleted from under the draft.
    let r = call(
        &w.app,
        "POST",
        "/api/ticket-drafts",
        Some(intake(vec![id])),
        Some(&w.requester),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let draft = r.body["id"].as_str().unwrap().to_owned();
    let r = raw(&w.app, "DELETE", &paths[0], &w.requester, None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    // A failed submission leaves it pending and the draft intact.
    sqlx::query("UPDATE categories SET archived_at = now()")
        .execute(&w.db.pool)
        .await
        .unwrap();
    let submit = json!({"idempotency_key": Uuid::new_v4(), "version": 1});
    let r = call(
        &w.app,
        "POST",
        &format!("/api/ticket-drafts/{draft}/submit"),
        Some(submit.clone()),
        Some(&w.requester),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.body);
    let attached: Option<Uuid> =
        sqlx::query_scalar("SELECT ticket_id FROM screenshots WHERE id = $1")
            .bind(id)
            .fetch_one(&w.db.pool)
            .await
            .unwrap();
    assert_eq!(attached, None);
    sqlx::query("UPDATE categories SET archived_at = NULL")
        .execute(&w.db.pool)
        .await
        .unwrap();
    let r = call(
        &w.app,
        "POST",
        &format!("/api/ticket-drafts/{draft}/submit"),
        Some(submit),
        Some(&w.requester),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let ticket = r.body["id"].as_str().unwrap().to_owned();
    let r = call(
        &w.app,
        "GET",
        &format!("/api/tickets/{ticket}"),
        None,
        Some(&w.requester),
    )
    .await;
    assert_eq!(r.body["intake_snapshot"]["screenshot_ids"], json!([id]));

    // Frozen: no deletion, no reuse on another ticket, not even through SQL.
    let r = raw(&w.app, "DELETE", &paths[0], &w.requester, None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let body = json!({"idempotency_key": Uuid::new_v4(), "intake": intake(vec![id])});
    let r = call(
        &w.app,
        "POST",
        "/api/tickets",
        Some(body),
        Some(&w.requester),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    for sql in [
        "DELETE FROM screenshots WHERE id = $1",
        "UPDATE screenshots SET ticket_id = NULL, attached_at = NULL WHERE id = $1",
        "UPDATE screenshots SET filename = 'other.png' WHERE id = $1",
    ] {
        let err = sqlx::query(sql)
            .bind(id)
            .execute(&w.db.pool)
            .await
            .unwrap_err();
        assert_eq!(
            err.as_database_error().unwrap().code().as_deref(),
            Some("23001"),
            "{sql}"
        );
    }
    let audits: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM audit_events WHERE event_type LIKE 'screenshot%' ORDER BY id",
    )
    .fetch_all(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(audits, ["screenshot_uploaded"]);
}

#[tokio::test]
async fn attached_screenshots_follow_current_ticket_visibility() {
    let Some(w) = world().await else { return };
    let pool = &w.db.pool;
    let lead = fixtures::account(pool, Role::TeamLead, "lead").await;
    let member = fixtures::account(pool, Role::Operator, "member").await;
    let other_lead = fixtures::account(pool, Role::TeamLead, "other_lead").await;
    fixtures::account(pool, Role::Operator, "dispatcher").await;
    fixtures::account(pool, Role::User, "other").await;
    let desk = fixtures::team(pool, "Desk", lead).await.unwrap();
    fixtures::member(pool, desk, member).await.unwrap();
    let network = fixtures::team(pool, "Network", other_lead).await.unwrap();
    let mut cookies = std::collections::HashMap::new();
    for name in [
        "admin",
        "lead",
        "member",
        "other_lead",
        "dispatcher",
        "other",
    ] {
        cookies.insert(name, login(&w.app, name, PASSWORD).await.session());
    }
    cookies.insert("requester", w.requester.clone());

    // Direct submission (no draft) attaches too.
    let id = upload_png(&w, &w.requester).await;
    let body = json!({"idempotency_key": Uuid::new_v4(), "intake": {"category_id": w.category,
        "priority_id": w.priority, "subject": "Crash", "description": "Help", "screenshot_ids": [id]}});
    let r = call(
        &w.app,
        "POST",
        "/api/tickets",
        Some(body),
        Some(&w.requester),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let ticket: Uuid = r.body["id"].as_str().unwrap().parse().unwrap();

    let check = async |label: &str, allowed: &[&str]| {
        for (name, cookie) in &cookies {
            let r = raw(
                &w.app,
                "GET",
                &format!("/api/screenshots/{id}/preview"),
                cookie,
                None,
            )
            .await;
            let expected = if allowed.contains(name) {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            };
            assert_eq!(r.status, expected, "{label}: {name}");
        }
    };
    // General queue: every staff role, never other requesters.
    check(
        "general queue",
        &[
            "requester",
            "admin",
            "lead",
            "member",
            "other_lead",
            "dispatcher",
        ],
    )
    .await;
    // Dispatched: only the current team and Admins; the dispatcher loses access.
    let route = |team: Option<Uuid>| {
        sqlx::query("UPDATE tickets SET team_id = $2 WHERE id = $1")
            .bind(ticket)
            .bind(team)
            .execute(pool)
    };
    route(Some(desk)).await.unwrap();
    check("desk", &["requester", "admin", "lead", "member"]).await;
    // Membership removal revokes immediately.
    sqlx::query("DELETE FROM team_memberships WHERE account_id = $1")
        .bind(member)
        .execute(pool)
        .await
        .unwrap();
    check("member removed", &["requester", "admin", "lead"]).await;
    // Transfer revokes the previous team.
    route(Some(network)).await.unwrap();
    check("transferred", &["requester", "admin", "other_lead"]).await;
    // AI-solved history: the owner and Admins only, read-only.
    route(None).await.unwrap();
    sqlx::query("UPDATE tickets SET status = 'ai_solved', resolution_source = 'ai', closure_reason = 'requester_confirmed' WHERE id = $1")
        .bind(ticket).execute(pool).await.unwrap();
    check("ai solved", &["requester", "admin"]).await;
    let r = raw(
        &w.app,
        "DELETE",
        &format!("/api/screenshots/{id}"),
        &w.requester,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = raw(
        &w.app,
        "DELETE",
        &format!("/api/screenshots/{id}"),
        &cookies["admin"],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let status: String = sqlx::query_scalar("SELECT status::text FROM tickets WHERE id = $1")
        .bind(ticket)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(status, "ai_solved");
}

#[tokio::test]
async fn orphans_expire_unless_a_draft_holds_them_and_stray_files_are_swept() {
    let Some(w) = world().await else { return };
    let pool = &w.db.pool;
    let dir = screenshot_dir(pool);
    let orphan = upload_png(&w, &w.requester).await;
    let held = upload_png(&w, &w.requester).await;
    let fresh = upload_png(&w, &w.requester).await;
    let removed = upload_png(&w, &w.requester).await;
    let r = call(
        &w.app,
        "POST",
        "/api/ticket-drafts",
        Some(json!({"screenshot_ids": [held]})),
        Some(&w.requester),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    // Explicit removal of an unreferenced own upload is immediate.
    let r = raw(
        &w.app,
        "DELETE",
        &format!("/api/screenshots/{removed}"),
        &w.requester,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(!dir.join(removed.to_string()).exists());
    let r = raw(
        &w.app,
        "GET",
        &format!("/api/screenshots/{removed}/preview"),
        &w.requester,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    sqlx::query(
        "UPDATE screenshots SET touched_at = now() - interval '25 hours' WHERE id = ANY($1)",
    )
    .bind(vec![orphan, held])
    .execute(pool)
    .await
    .unwrap();
    let stray = Uuid::new_v4();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 3600);
    for name in [stray.to_string(), format!("{}.tmp", Uuid::new_v4())] {
        std::fs::File::create(dir.join(&name))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    std::fs::File::open(dir.join(fresh.to_string()))
        .unwrap()
        .set_modified(old)
        .unwrap();

    let expired = screenshots::cleanup(pool, &dir).await.unwrap();
    assert_eq!(expired, [orphan]);
    let mut files = stored_files(&w);
    files.sort();
    let mut expected = vec![held.to_string(), fresh.to_string()];
    expected.sort();
    assert_eq!(
        files, expected,
        "orphan, stray, and temp files removed; records kept their files"
    );
    let audit: Value = sqlx::query_scalar(
        "SELECT metadata FROM audit_events WHERE event_type = 'screenshots_expired'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(audit, json!({"screenshot_ids": [orphan]}));
    // The draft-held upload survives every run while the draft references it.
    assert!(screenshots::cleanup(pool, &dir).await.unwrap().is_empty());
}

#[tokio::test]
async fn storage_failures_leave_no_misleading_records() {
    let Some(w) = world().await else { return };
    let blocked = screenshot_dir(&w.db.pool).with_extension("file");
    std::fs::write(&blocked, b"not a directory").unwrap();
    let broken = tikkit_api::app(AppState {
        pool: w.db.pool.clone(),
        screenshot_dir: blocked.join("screenshots").into(),
    });
    let r = upload(&broken, &w.requester, "x.png", "image/png", png()).await;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(r.json()["error"]["code"], "internal");
    assert_eq!(count(&w).await, 0);
    std::fs::remove_file(&blocked).unwrap();

    // A record whose file vanished fails closed instead of serving anything.
    let id = upload_png(&w, &w.requester).await;
    std::fs::remove_file(screenshot_dir(&w.db.pool).join(id.to_string())).unwrap();
    let r = raw(
        &w.app,
        "GET",
        &format!("/api/screenshots/{id}/preview"),
        &w.requester,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(r.json()["error"]["code"], "internal");
}

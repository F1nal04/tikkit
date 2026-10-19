//! Screenshot uploads, previews, and downloads. See docs/screenshots.md.
//!
//! Uploads are untrusted: the magic bytes pick PNG/JPEG/WebP, the declared type
//! and file extension must agree, and the image is fully decoded under pixel
//! limits and re-encoded, so only clean raster bytes (plus the colour profile)
//! are stored. Pending uploads belong to their uploader; [`attach`] binds them to
//! a ticket, after which every read follows [`authz::ticket_view`].
use std::{
    collections::HashSet,
    io::{Cursor, Write},
    path::{Path as FsPath, PathBuf},
    time::{Duration, SystemTime},
};

use axum::{
    body::Bytes,
    extract::{FromRequest, Request, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, StatusCode,
        header::{
            CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY, CONTENT_TYPE,
            X_CONTENT_TYPE_OPTIONS,
        },
    },
    response::{IntoResponse, Response},
};
use image::{
    DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageResult, Limits,
    codecs::{
        jpeg::{JpegDecoder, JpegEncoder},
        png::{PngDecoder, PngEncoder},
        webp::{WebPDecoder, WebPEncoder},
    },
    metadata::Orientation,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use tokio::sync::{Semaphore, SemaphorePermit};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, FieldErrors, Json, Path, Query},
    auth::{CurrentUser, audit, ident},
    authz::{self, Actor, TicketScope},
};

/// Largest accepted upload body.
pub const MAX_BYTES: usize = 10 * 1024 * 1024;
/// Largest decoded image (width × height), against decompression bombs.
pub const MAX_PIXELS: u64 = 40_000_000;
/// Longest accepted side in pixels.
pub const MAX_SIDE: u32 = 16_384;
/// Screenshots per submitted intake (and, later, per message).
pub const MAX_PER_TICKET: usize = 10;
/// Unattached uploads per account, including those referenced by drafts.
pub const MAX_PENDING: i64 = 50;
/// Unattached uploads not referenced by a draft are deleted after this long.
pub const ORPHAN_TTL: Duration = Duration::from_secs(24 * 3600);

// Bound buffered uploads and decoding together; wait fairly before reading bodies.
static DECODING: Semaphore = Semaphore::const_new(2);

/// Acquire capacity before buffering a request body. The decoder keeps the
/// permit even if its HTTP request is cancelled (spawn_blocking cannot abort).
pub struct UploadBody {
    bytes: Bytes,
    permit: SemaphorePermit<'static>,
}

impl FromRequest<AppState> for UploadBody {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &AppState) -> Result<Self, ApiError> {
        let permit = DECODING.acquire().await.map_err(|_| ApiError::Internal)?;
        let bytes =
            tokio::time::timeout(Duration::from_secs(30), Bytes::from_request(request, state))
                .await
                .map_err(|_| ApiError::BadRequest)?
                .map_err(|error| {
                    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                        ApiError::PayloadTooLarge
                    } else {
                        ApiError::BadRequest
                    }
                })?;
        Ok(Self { bytes, permit })
    }
}

impl UploadBody {
    async fn normalize(
        self,
        kind: Kind,
    ) -> Result<(Normalized, SemaphorePermit<'static>), ApiError> {
        blocking(move || {
            let permit = self.permit;
            normalize(&self.bytes, kind).map(|image| (image, permit))
        })
        .await?
        .map_err(|code| field("file", code))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Png,
    Jpeg,
    WebP,
}

impl Kind {
    fn detect(bytes: &[u8]) -> Option<Self> {
        match image::guess_format(bytes).ok()? {
            ImageFormat::Png => Some(Self::Png),
            ImageFormat::Jpeg => Some(Self::Jpeg),
            ImageFormat::WebP => Some(Self::WebP),
            _ => None,
        }
    }

    fn from_mime(mime: &str) -> Option<Self> {
        match mime.split(';').next()?.trim().to_ascii_lowercase().as_str() {
            "image/png" => Some(Self::Png),
            "image/jpeg" => Some(Self::Jpeg),
            "image/webp" => Some(Self::WebP),
            _ => None,
        }
    }

    fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::WebP => "image/webp",
        }
    }

    fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Png => &["png"],
            Self::Jpeg => &["jpg", "jpeg", "jpe", "jfif"],
            Self::WebP => &["webp"],
        }
    }
}

/// Metadata of a validated screenshot. The bytes are only served by the
/// preview/download endpoints.
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct Screenshot {
    pub id: Uuid,
    /// Normalized server-side; the extension matches `content_type`.
    pub filename: String,
    /// `image/png`, `image/jpeg`, or `image/webp`, from the decoded content.
    pub content_type: String,
    /// Size of the stored, re-encoded file.
    pub byte_size: i32,
    pub width: i32,
    pub height: i32,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

const COLUMNS: &str =
    "s.id, s.filename, s.content_type, s.byte_size, s.width, s.height, s.created_at";

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct UploadParams {
    /// Original file name; only a safe base name is kept. Its extension, if
    /// any, must match the image format.
    pub filename: Option<String>,
}

fn field(name: &str, code: &'static str) -> ApiError {
    ApiError::Validation(FieldErrors::from([(name.into(), vec![code])]))
}

fn invalid_ids() -> ApiError {
    field("screenshot_ids", "invalid")
}

struct Normalized {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
}

/// Fully decode untrusted bytes of `kind` and re-encode them in the same format,
/// dropping everything but pixels and the colour profile. Error = field code.
fn normalize(bytes: &[u8], kind: Kind) -> Result<Normalized, &'static str> {
    let corrupt = |_| "corrupt";
    let reader = Cursor::new(bytes);
    let mut decoder: Box<dyn ImageDecoder + '_> = match kind {
        Kind::Png => {
            // set_limits() does not update the PNG reader's internal limits.
            // Bound metadata inflation during construction, before pixel decoding.
            let mut limits = Limits::default();
            limits.max_image_width = Some(MAX_SIDE);
            limits.max_image_height = Some(MAX_SIDE);
            limits.max_alloc = Some(16 << 20);
            let decoder = PngDecoder::with_limits(reader, limits).map_err(|error| {
                if matches!(error, image::ImageError::Limits(_)) {
                    "dimensions_too_large"
                } else {
                    "corrupt"
                }
            })?;
            if decoder.is_apng().map_err(corrupt)? {
                return Err("animated");
            }
            Box::new(decoder)
        }
        Kind::Jpeg => Box::new(JpegDecoder::new(reader).map_err(corrupt)?),
        Kind::WebP => {
            validate_webp_chunks(bytes)?;
            let decoder = WebPDecoder::new(reader).map_err(corrupt)?;
            if decoder.has_animation() {
                return Err("animated");
            }
            Box::new(decoder)
        }
    };
    // Header dimensions are checked before any pixel buffer is allocated.
    let (width, height) = decoder.dimensions();
    if width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err("dimensions_too_large");
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_PIXELS * 8 + (64 << 20));
    decoder
        .set_limits(limits)
        .map_err(|_| "dimensions_too_large")?;
    let icc = decoder.icc_profile().ok().flatten();
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(corrupt)?;
    image.apply_orientation(orientation);
    let mut out = Vec::new();
    match kind {
        Kind::Png => encode(PngEncoder::new(&mut out), icc, &image),
        Kind::Jpeg => encode(
            JpegEncoder::new_with_quality(&mut out, 90),
            icc,
            &DynamicImage::ImageRgb8(image.to_rgb8()),
        ),
        Kind::WebP if image.color().has_alpha() => encode(
            WebPEncoder::new_lossless(&mut out),
            icc,
            &DynamicImage::ImageRgba8(image.to_rgba8()),
        ),
        Kind::WebP => encode(
            WebPEncoder::new_lossless(&mut out),
            icc,
            &DynamicImage::ImageRgb8(image.to_rgb8()),
        ),
    }
    .map_err(corrupt)?;
    Ok(Normalized {
        bytes: out,
        width: image.width(),
        height: image.height(),
    })
}

// The WebP decoder allocates metadata using the declared chunk length before
// reading it, and its image::Limits do not constrain those allocations.
fn validate_webp_chunks(bytes: &[u8]) -> Result<(), &'static str> {
    let size = bytes.get(4..8).ok_or("corrupt")?;
    let end = u64::from(u32::from_le_bytes(size.try_into().unwrap())) + 8;
    if end < 12 || end > bytes.len() as u64 {
        return Err("corrupt");
    }
    let mut offset = 12u64;
    while offset < end {
        if end - offset < 8 {
            return Err("corrupt");
        }
        let size = &bytes[offset as usize + 4..offset as usize + 8];
        let size = u64::from(u32::from_le_bytes(size.try_into().unwrap()));
        offset += 8 + size + size % 2;
        if offset > end {
            return Err("corrupt");
        }
    }
    Ok(())
}

fn encode(
    mut encoder: impl ImageEncoder,
    icc: Option<Vec<u8>>,
    image: &DynamicImage,
) -> ImageResult<()> {
    if let Some(icc) = icc {
        // Encoders without ICC support drop it; the pixels are unaffected.
        let _ = encoder.set_icc_profile(icc);
    }
    image.write_with_encoder(encoder)
}

/// Safe base name with the canonical extension of `kind`. A present extension
/// (1-5 ASCII alphanumerics with a letter) must belong to `kind`.
fn normalize_filename(raw: Option<&str>, kind: Kind) -> Result<String, &'static str> {
    let base = raw
        .unwrap_or_default()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let stem = match base.rsplit_once('.') {
        Some((stem, ext))
            if (1..=5).contains(&ext.len())
                && ext.bytes().all(|b| b.is_ascii_alphanumeric())
                && ext.bytes().any(|b| b.is_ascii_alphabetic()) =>
        {
            if !kind
                .extensions()
                .contains(&ext.to_ascii_lowercase().as_str())
            {
                return Err("type_mismatch");
            }
            stem
        }
        _ => base,
    };
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    let clean = clean.trim_matches([' ', '.']);
    let clean = if clean.is_empty() {
        "screenshot"
    } else {
        clean
    };
    Ok(format!("{clean}.{}", kind.extensions()[0]))
}

fn file_path(dir: &FsPath, id: Uuid) -> PathBuf {
    dir.join(id.to_string())
}

/// Durable write: temp file, fsync, atomic rename. Never leaves a partial `<id>`.
fn write_file(dir: &FsPath, id: Uuid, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{id}.tmp"));
    let written = std::fs::File::create_new(&tmp)
        .and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, file_path(dir, id)))
        .and_then(|()| std::fs::File::open(dir)?.sync_all());
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(file_path(dir, id));
    }
    written
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::Internal)
}

/// Upload one screenshot as the raw request body (`Content-Type: image/png`,
/// `image/jpeg`, or `image/webp`). It stays private to the uploader until
/// attached by submission; unreferenced uploads expire after 24 hours.
#[utoipa::path(post, path = "/api/screenshots", tag = "screenshots", params(UploadParams),
    request_body(description = "Raw image bytes, at most 10 MiB", content(("image/png"), ("image/jpeg"), ("image/webp"))),
    responses(
        (status = 201, body = Screenshot),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 413, description = "`payload_too_large` (over 10 MiB)", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed`: `file` is `required`, `unsupported_type`, `type_mismatch`, `corrupt`, `animated`, `dimensions_too_large`, or `too_many`; `filename` is `type_mismatch`", body = crate::api::ErrorEnvelope),
        (status = 400, description = "`bad_request` (incomplete upload or body read exceeded 30 seconds)", body = crate::api::ErrorEnvelope),
        (status = 500, description = "`internal` (storage or database failure; a commit acknowledgement may be lost)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn upload(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(params): Query<UploadParams>,
    headers: HeaderMap,
    body: UploadBody,
) -> Result<(StatusCode, Json<Screenshot>), ApiError> {
    let declared = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(Kind::from_mime)
        .ok_or_else(|| field("file", "unsupported_type"))?;
    if body.bytes.is_empty() {
        return Err(field("file", "required"));
    }
    let kind = Kind::detect(&body.bytes).ok_or_else(|| field("file", "unsupported_type"))?;
    if kind != declared {
        return Err(field("file", "type_mismatch"));
    }
    let filename =
        normalize_filename(params.filename.as_deref(), kind).map_err(|c| field("filename", c))?;
    let mut tx = state.pool.begin().await?;
    // Share the quota across API processes, not just the decoding semaphore.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('screenshot_upload'), hashtext($1))")
        .bind(user.account_id().to_string())
        .execute(&mut *tx)
        .await?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM screenshots WHERE uploader_account_id = $1 AND ticket_id IS NULL",
    )
    .bind(user.account_id())
    .fetch_one(&mut *tx)
    .await?;
    if pending >= MAX_PENDING {
        return Err(field("file", "too_many"));
    }
    let (image, permit) = body.normalize(kind).await?;

    let id = Uuid::new_v4();
    let dir = state.screenshot_dir.clone();
    let bytes = image.bytes;
    let byte_size =
        i32::try_from(bytes.len()).map_err(|_| field("file", "dimensions_too_large"))?;
    let (written, _permit) = blocking(move || (write_file(&dir, id, &bytes), permit)).await?;
    if let Err(error) = written {
        tracing::error!(%error, "screenshot storage write failed");
        return Err(ApiError::Internal);
    }
    let screenshot: Screenshot = sqlx::query_as(&format!(
        "INSERT INTO screenshots AS s (id, uploader_account_id, filename, content_type, byte_size, width, height)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {COLUMNS}"))
        .bind(id).bind(user.account_id()).bind(&filename).bind(kind.mime()).bind(byte_size)
        .bind(image.width as i32).bind(image.height as i32).fetch_one(&mut *tx).await?;
    audit(
        &mut *tx,
        "screenshot_uploaded",
        Some(user.account_id()),
        Some(user.account_id()),
        json!({"screenshot_id": id, "content_type": kind.mime(), "byte_size": byte_size}),
    )
    .await?;
    // A commit error can mean a lost acknowledgement after a successful commit.
    // Leave the file for cleanup, which checks committed records after the TTL.
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(screenshot)))
}

#[derive(sqlx::FromRow)]
struct AccessRow {
    #[sqlx(flatten)]
    screenshot: Screenshot,
    uploader_account_id: Uuid,
    ticket_id: Option<Uuid>,
    requester_account_id: Option<Uuid>,
    team_id: Option<Uuid>,
    assignee_account_id: Option<Uuid>,
    status: Option<String>,
}

/// The screenshot if the caller may read it now: a pending upload only by its
/// uploader; an attached one under the ticket's current visibility (team
/// isolation, AI-solved records for the requester and Admins only). Authorship
/// grants nothing once attached. Otherwise `404`, same as a missing ID.
async fn readable(pool: &PgPool, user: &CurrentUser, id: Uuid) -> Result<Screenshot, ApiError> {
    let row: AccessRow = sqlx::query_as(&format!(
        "SELECT {COLUMNS}, s.uploader_account_id, s.ticket_id, t.requester_account_id, t.team_id,
                t.assignee_account_id, t.status::text AS status
         FROM screenshots s LEFT JOIN tickets t ON t.id = s.ticket_id WHERE s.id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(ApiError::NotFound)?;
    let allowed = match (row.ticket_id, row.requester_account_id, row.status) {
        (None, ..) => row.uploader_account_id == user.account_id(),
        (Some(_), Some(requester_account_id), Some(status)) => {
            let actor = Actor::load(pool, user).await?;
            let scope = TicketScope {
                requester_account_id,
                team_id: row.team_id,
                assignee_account_id: row.assignee_account_id,
                status: ident(status)?,
            };
            authz::ticket_view(&actor, &scope).is_some()
        }
        _ => false,
    };
    if allowed {
        Ok(row.screenshot)
    } else {
        Err(ApiError::NotFound)
    }
}

/// Metadata of a screenshot the caller may currently read.
#[utoipa::path(get, path = "/api/screenshots/{id}", tag = "screenshots", params(("id" = Uuid, Path)),
    responses((status = 200, body = Screenshot),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found` (missing or not visible)", body = crate::api::ErrorEnvelope)))]
pub async fn metadata(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    Ok((
        [(CACHE_CONTROL, "private, no-store")],
        Json(readable(&state.pool, &user, id).await?),
    )
        .into_response())
}

/// Inline image for `<img>` previews. Access is rechecked on every request and
/// never cached, so revoked access applies to old URLs immediately.
#[utoipa::path(get, path = "/api/screenshots/{id}/preview", tag = "screenshots", params(("id" = Uuid, Path)),
    responses((status = 200, description = "Validated image bytes", content(("image/png"), ("image/jpeg"), ("image/webp"))),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found` (missing or not visible)", body = crate::api::ErrorEnvelope)))]
pub async fn preview(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    serve(&state, &user, id, "inline").await
}

/// The same bytes as an attachment download.
#[utoipa::path(get, path = "/api/screenshots/{id}/download", tag = "screenshots", params(("id" = Uuid, Path)),
    responses((status = 200, description = "Validated image bytes (`Content-Disposition: attachment`)", content(("image/png"), ("image/jpeg"), ("image/webp"))),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found` (missing or not visible)", body = crate::api::ErrorEnvelope)))]
pub async fn download(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    serve(&state, &user, id, "attachment").await
}

async fn serve(
    state: &AppState,
    user: &CurrentUser,
    id: Uuid,
    disposition: &str,
) -> Result<Response, ApiError> {
    let screenshot = readable(&state.pool, user, id).await?;
    let path = file_path(&state.screenshot_dir, id);
    let bytes = blocking(move || std::fs::read(path))
        .await?
        .map_err(|error| {
            tracing::error!(%error, %id, "screenshot file unreadable");
            ApiError::Internal
        })?;
    let name = &screenshot.filename;
    let ascii: String = name
        .chars()
        .map(|c| if c.is_ascii() { c } else { '_' })
        .collect();
    let disposition = format!(
        "{disposition}; filename=\"{ascii}\"; filename*=UTF-8''{}",
        percent_encode(name)
    );
    let header = |v: &str| HeaderValue::from_str(v).map_err(|_| ApiError::Internal);
    Ok((
        [
            (CONTENT_TYPE, header(&screenshot.content_type)?),
            (CONTENT_DISPOSITION, header(&disposition)?),
            (X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
            (
                CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'; sandbox"),
            ),
            (CACHE_CONTROL, HeaderValue::from_static("private, no-store")),
            (
                HeaderName::from_static("cross-origin-resource-policy"),
                HeaderValue::from_static("same-origin"),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// RFC 5987 `attr-char` encoding for `filename*`.
fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Delete an own pending upload immediately. Attached screenshots are frozen
/// (`409`); so are uploads still referenced by an editable draft: save the draft
/// without it first (or just save it; unreferenced uploads expire on their own).
#[utoipa::path(delete, path = "/api/screenshots/{id}", tag = "screenshots", params(("id" = Uuid, Path)),
    responses((status = 204, description = "Deleted"),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found` (missing or another account's upload)", body = crate::api::ErrorEnvelope),
        (status = 409, description = "`conflict` (attached to a ticket or referenced by a draft)", body = crate::api::ErrorEnvelope)))]
pub async fn delete(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.pool.begin().await?;
    let attached: Option<bool> = sqlx::query_scalar(
        "SELECT ticket_id IS NOT NULL FROM screenshots WHERE id = $1 AND uploader_account_id = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(user.account_id())
    .fetch_optional(&mut *tx)
    .await?;
    match attached {
        None => return Err(ApiError::NotFound),
        Some(true) => return Err(ApiError::Conflict),
        Some(false) => {}
    }
    let referenced: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM ticket_drafts WHERE requester_account_id = $2
                        AND intake->'screenshot_ids' ? $1::text)",
    )
    .bind(id)
    .bind(user.account_id())
    .fetch_one(&mut *tx)
    .await?;
    if referenced {
        return Err(ApiError::Conflict);
    }
    sqlx::query("DELETE FROM screenshots WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut *tx,
        "screenshot_deleted",
        Some(user.account_id()),
        Some(user.account_id()),
        json!({"screenshot_id": id}),
    )
    .await?;
    tx.commit().await?;
    // A failed removal leaves a stray file for `cleanup`; the record is already gone.
    let path = file_path(&state.screenshot_dir, id);
    let _ = blocking(move || std::fs::remove_file(path)).await;
    Ok(StatusCode::NO_CONTENT)
}

fn distinct(ids: &[Uuid]) -> bool {
    ids.iter().collect::<HashSet<_>>().len() == ids.len()
}

/// Field errors for a screenshot ID list before touching the database.
pub fn check_ids(errors: &mut FieldErrors, ids: &[Uuid]) {
    if ids.len() > MAX_PER_TICKET {
        errors.insert("screenshot_ids".into(), vec!["too_many"]);
    } else if !distinct(ids) {
        errors.insert("screenshot_ids".into(), vec!["invalid"]);
    }
}

/// Draft save: every ID must be a pending upload of `uploader`. Refreshes their
/// expiry clock under a row lock, so a concurrent [`cleanup`] cannot remove them.
pub async fn keep_pending(
    db: &mut PgConnection,
    uploader: Uuid,
    ids: &[Uuid],
) -> Result<(), ApiError> {
    if ids.is_empty() {
        return Ok(());
    }
    let touched = sqlx::query(
        "UPDATE screenshots SET touched_at = now()
         WHERE id = ANY($1) AND uploader_account_id = $2 AND ticket_id IS NULL",
    )
    .bind(ids)
    .bind(uploader)
    .execute(db)
    .await?
    .rows_affected();
    if touched as usize == ids.len() && distinct(ids) {
        Ok(())
    } else {
        Err(invalid_ids())
    }
}

/// Bind pending uploads of `uploader` to `ticket_id`, once and for good, inside
/// the caller's transaction. Each ID must be an unattached upload of `uploader`
/// (a concurrent attach of the same ID loses): otherwise `422 screenshot_ids:
/// invalid`. Visibility from then on is the ticket's. The caller decides whether
/// the ticket accepts new content (status, lifecycle, message permissions).
pub async fn attach(
    db: &mut PgConnection,
    uploader: Uuid,
    ids: &[Uuid],
    ticket_id: Uuid,
) -> Result<(), ApiError> {
    if ids.is_empty() {
        return Ok(());
    }
    if ids.len() > MAX_PER_TICKET || !distinct(ids) {
        return Err(invalid_ids());
    }
    let attached = sqlx::query(
        "UPDATE screenshots SET ticket_id = $3, attached_at = now()
         WHERE id = ANY($1) AND uploader_account_id = $2 AND ticket_id IS NULL",
    )
    .bind(ids)
    .bind(uploader)
    .bind(ticket_id)
    .execute(db)
    .await?
    .rows_affected();
    if attached as usize == ids.len() {
        Ok(())
    } else {
        Err(invalid_ids())
    }
}

/// Delete expired pending uploads (unreferenced by any draft for [`ORPHAN_TTL`])
/// and stray files without a record (crash between write and insert). Safe to
/// run concurrently from several API processes. Returns the expired uploads.
pub async fn cleanup(pool: &PgPool, dir: &FsPath) -> Result<Vec<Uuid>, ApiError> {
    let mut tx = pool.begin().await?;
    // ponytail: scans draft JSON per candidate; index draft screenshot refs if drafts grow huge.
    let expired: Vec<Uuid> = sqlx::query_scalar(
        "DELETE FROM screenshots s
         WHERE s.ticket_id IS NULL AND s.touched_at < now() - make_interval(secs => $1)
           AND NOT EXISTS (SELECT 1 FROM ticket_drafts d WHERE d.intake->'screenshot_ids' ? s.id::text)
         RETURNING s.id",
    )
    .bind(ORPHAN_TTL.as_secs_f64())
    .fetch_all(&mut *tx)
    .await?;
    if !expired.is_empty() {
        audit(
            &mut *tx,
            "screenshots_expired",
            None,
            None,
            json!({"screenshot_ids": expired}),
        )
        .await?;
    }
    tx.commit().await?;

    let owned = dir.to_owned();
    let gone = expired.clone();
    let stale: Vec<(Uuid, PathBuf)> = blocking(move || {
        for id in gone {
            let _ = std::fs::remove_file(file_path(&owned, id));
        }
        let cutoff = SystemTime::now() - ORPHAN_TTL;
        let Ok(entries) = std::fs::read_dir(&owned) else {
            return Vec::new();
        };
        entries
            .filter_map(|e| {
                let e = e.ok()?;
                let name = e.file_name().into_string().ok()?;
                let id = name.strip_suffix(".tmp").unwrap_or(&name).parse().ok()?;
                (e.metadata().ok()?.modified().ok()? < cutoff).then(|| (id, e.path()))
            })
            .collect()
    })
    .await?;
    if !stale.is_empty() {
        let ids: Vec<Uuid> = stale.iter().map(|(id, _)| *id).collect();
        let known: HashSet<Uuid> =
            sqlx::query_scalar("SELECT id FROM screenshots WHERE id = ANY($1)")
                .bind(&ids)
                .fetch_all(pool)
                .await?
                .into_iter()
                .collect();
        blocking(move || {
            for (id, path) in stale {
                let tmp = path.extension().is_some();
                if tmp || !known.contains(&id) {
                    let _ = std::fs::remove_file(path);
                }
            }
        })
        .await?;
    }
    Ok(expired)
}

/// Runs [`cleanup`] at startup and then hourly.
pub async fn cleanup_loop(pool: PgPool, dir: PathBuf) {
    let mut every = tokio::time::interval(Duration::from_secs(3600));
    loop {
        every.tick().await;
        match cleanup(&pool, &dir).await {
            Ok(expired) if !expired.is_empty() => {
                tracing::info!(count = expired.len(), "expired orphaned screenshots");
            }
            Ok(_) => {}
            Err(_) => tracing::warn!("screenshot cleanup failed; retrying in an hour"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_capacity_waits_before_buffering_and_survives_cancellation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let pool = sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://tikkit@127.0.0.1:1/tikkit")
                .unwrap();
            let state = AppState {
                pool,
                screenshot_dir: std::env::temp_dir().into(),
            };
            let body = || Request::new(axum::body::Body::from(vec![0u8; 32]));
            let first = UploadBody::from_request(body(), &state).await.unwrap();
            let second = UploadBody::from_request(body(), &state).await.unwrap();
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(10),
                    UploadBody::from_request(body(), &state)
                )
                .await
                .is_err()
            );
            let (release, wait) = std::sync::mpsc::channel();
            let (started, ready) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = wait.recv();
            });
            ready.await.unwrap();
            let mut decode = Box::pin(first.normalize(Kind::Png));
            tokio::select! {
                biased;
                _ = &mut decode => panic!("decoder must be queued behind the blocking worker"),
                () = std::future::ready(()) => {}
            }
            drop(decode);
            assert_eq!(
                DECODING.available_permits(),
                0,
                "cancelled HTTP future must not free a running decoder's capacity"
            );
            release.send(()).unwrap();
            blocker.await.unwrap();
            let _finished = tokio::time::timeout(Duration::from_secs(2), DECODING.acquire())
                .await
                .unwrap()
                .unwrap();
            drop(second);
        });
    }

    #[test]
    fn webp_chunk_lengths_must_fit_the_uploaded_bytes() {
        let mut frame = Vec::new();
        DynamicImage::new_rgb8(2, 2)
            .write_to(&mut Cursor::new(&mut frame), ImageFormat::WebP)
            .unwrap();
        let mut bytes = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\x20\0\0\0\x01\0\0\x01\0\0".to_vec();
        bytes.extend_from_slice(&frame[12..]);
        bytes.extend_from_slice(b"ICCP");
        bytes.extend_from_slice(&(16u32 * 1024 * 1024).to_le_bytes());
        let len = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&len.to_le_bytes());
        assert!(
            normalize(&bytes, Kind::WebP).is_err(),
            "a truncated metadata chunk must not allocate its claimed length or be silently dropped"
        );
    }

    #[test]
    fn compressed_icc_profiles_cannot_expand_without_a_constructor_limit() {
        let pixels = DynamicImage::new_rgb8(2, 2);
        let mut bytes = Vec::new();
        let mut encoder = PngEncoder::new(&mut bytes);
        encoder.set_icc_profile(vec![42; 17 * 1024 * 1024]).unwrap();
        pixels.write_with_encoder(encoder).unwrap();
        assert!(bytes.len() < MAX_BYTES);
        let clean = normalize(&bytes, Kind::Png).unwrap();
        let mut stored = PngDecoder::new(Cursor::new(&clean.bytes)).unwrap();
        assert!(
            stored.icc_profile().unwrap().is_none(),
            "oversized metadata must not survive normalization"
        );
    }

    #[test]
    fn filenames_are_normalized_and_extensions_must_match() {
        let png = |raw| normalize_filename(raw, Kind::Png);
        assert_eq!(png(Some("C:\\Users\\me\\shot.PNG")).unwrap(), "shot.png");
        assert_eq!(png(Some("../../etc/passwd")).unwrap(), "passwd.png");
        assert_eq!(
            png(Some("Bildschirmfoto 10.00.00")).unwrap(),
            "Bildschirmfoto 10.00.00.png"
        );
        assert_eq!(
            png(Some("Übersicht<script>.png")).unwrap(),
            "Übersicht_script_.png"
        );
        assert_eq!(png(Some("evil\u{202e}gnp.exe")), Err("type_mismatch"));
        assert_eq!(png(Some("x.svg")), Err("type_mismatch"));
        assert_eq!(png(Some("Ü\"\r\nshot.png")).unwrap(), "Ü___shot.png");
        assert_eq!(png(Some(".png")).unwrap(), "screenshot.png");
        assert_eq!(png(None).unwrap(), "screenshot.png");
        let long = "a".repeat(500);
        assert_eq!(png(Some(&long)).unwrap().len(), 104);
        assert_eq!(
            normalize_filename(Some("a.JPEG"), Kind::Jpeg).unwrap(),
            "a.jpg"
        );
        assert_eq!(percent_encode("Ü b.png"), "%C3%9C%20b.png");
    }
}

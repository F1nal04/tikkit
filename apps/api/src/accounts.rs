//! Invitations, password recovery, profile, and Admin account lifecycle. See docs/auth.md.

use std::sync::Arc;

use axum::{
    Extension,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, FieldErrors, Json, Language, Page, Pagination, Path, Query, Role},
    auth::{
        Account, Auth, Client, CurrentUser, audit, blocking, check_password, check_username,
        create_session, end_presented_session, hash_password, ident, ident_str, load_account,
        new_token, revoke_all_sessions, signed_in, token_hash, unique_violation, valid_email,
    },
    authz::{Permission, require_permission},
};

const INVITATION_DEFAULT_HOURS: u32 = 168;
const INVITATION_MAX_HOURS: u32 = 720;
const RESET_TTL: &str = "interval '1 hour'";
/// At most one recovery email per account in this window, whatever the rate limits allow.
const RESET_COOLDOWN: &str = "interval '15 minutes'";

fn field_error(field: &str, code: &'static str) -> ApiError {
    ApiError::Validation(FieldErrors::from([(field.into(), vec![code])]))
}

// ---------------------------------------------------------------- invitations

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateInvitationRequest {
    /// Role of the account created from this invitation. Default `user`.
    pub role: Option<Role>,
    /// Optional recipient. With SMTP configured the link is emailed there.
    pub email: Option<String>,
    /// Language of the email and preselected account language. Default `en`.
    pub language: Option<Language>,
    /// Lifetime in hours, 1-720. Default 168 (7 days).
    pub expires_in_hours: Option<u32>,
}

#[derive(Serialize, ToSchema)]
pub struct Invitation {
    pub id: Uuid,
    pub role: Role,
    #[schema(required = true)]
    pub email: Option<String>,
    pub language: Language,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: OffsetDateTime,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedInvitation {
    pub invitation: Invitation,
    /// Single-use acceptance link. Shown only now; only its hash is stored.
    pub url: String,
    /// Whether an email was queued (SMTP configured and an email given).
    pub emailed: bool,
}

#[derive(sqlx::FromRow)]
struct InvitationRow {
    id: Uuid,
    role: String,
    email: Option<String>,
    language: String,
    created_at: OffsetDateTime,
    expires_at: OffsetDateTime,
}

impl TryFrom<InvitationRow> for Invitation {
    type Error = ApiError;
    fn try_from(r: InvitationRow) -> Result<Self, ApiError> {
        Ok(Self {
            id: r.id,
            role: ident(r.role)?,
            email: r.email,
            language: ident(r.language)?,
            created_at: r.created_at,
            expires_at: r.expires_at,
        })
    }
}

const INVITATION_COLUMNS: &str = "id, role::text AS role, email, language, created_at, expires_at";
/// Usable: not accepted, not revoked, not expired.
const PENDING: &str = "accepted_at IS NULL AND revoked_at IS NULL AND expires_at > now()";

/// Creates a single-use invitation (Admin only). The role is fixed here and
/// applied on acceptance; the invitee cannot change it.
#[utoipa::path(post, path = "/api/invitations", tag = "accounts", request_body = CreateInvitationRequest,
    responses(
        (status = 201, body = CreatedInvitation),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`email`, `expires_in_hours`: `invalid`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn create_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    user: CurrentUser,
    Json(body): Json<CreateInvitationRequest>,
) -> Result<(StatusCode, Json<CreatedInvitation>), ApiError> {
    require_permission(&user, Permission::ManageAccounts)?;
    let role = body.role.unwrap_or(Role::User);
    let language = body.language.unwrap_or(Language::En);
    let hours = body.expires_in_hours.unwrap_or(INVITATION_DEFAULT_HOURS);
    let email = body
        .email
        .map(|e| e.trim().to_owned())
        .filter(|e| !e.is_empty());
    let mut errors = FieldErrors::new();
    if !(1..=INVITATION_MAX_HOURS).contains(&hours) {
        errors.insert("expires_in_hours".into(), vec!["invalid"]);
    }
    if email.as_deref().is_some_and(|e| !valid_email(e)) {
        errors.insert("email".into(), vec!["invalid"]);
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let token = new_token();
    let mut tx = state.pool.begin().await?;
    let row: InvitationRow = sqlx::query_as(&format!(
        "INSERT INTO invitations (token_hash, role, email, language, created_by, expires_at)
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(hours => $6))
         RETURNING {INVITATION_COLUMNS}"
    ))
    .bind(token_hash(&token))
    .bind(ident_str(role))
    .bind(&email)
    .bind(ident_str(language))
    .bind(user.account_id())
    .bind(hours as i32)
    .fetch_one(&mut *tx)
    .await?;
    let invitation = Invitation::try_from(row)?;
    audit(
        &mut *tx,
        "invitation_created",
        Some(user.account_id()),
        None,
        json!({ "invitation_id": invitation.id, "role": role, "expires_in_hours": hours }),
    )
    .await?;
    tx.commit().await?;

    let url = format!(
        "{}/invite/{token}?lang={}",
        auth.settings.public_origin,
        ident_str(language)
    );
    let emailed = email.as_deref().is_some_and(|to| {
        let (subject, text) = invitation_email(language, &url, hours);
        auth.settings.mailer.send(to, subject, text)
    });
    let created = CreatedInvitation {
        invitation,
        url,
        emailed,
    };
    Ok((StatusCode::CREATED, Json(created)))
}

fn invitation_email(language: Language, url: &str, hours: u32) -> (&'static str, String) {
    match language {
        Language::En => (
            "You are invited to Tikkit",
            format!(
                "An administrator invited you to create a Tikkit account.\n\n\
                 Open this link to choose your username and password:\n{url}\n\n\
                 The link works once and expires in {hours} hours.\n\
                 If you did not expect this invitation, ignore this email."
            ),
        ),
        Language::De => (
            "Einladung zu Tikkit",
            format!(
                "Sie wurden eingeladen, ein Tikkit-Konto anzulegen.\n\n\
                 Öffnen Sie diesen Link, um Benutzernamen und Passwort festzulegen:\n{url}\n\n\
                 Der Link funktioniert einmal und läuft in {hours} Stunden ab.\n\
                 Wenn Sie diese Einladung nicht erwartet haben, ignorieren Sie diese E-Mail."
            ),
        ),
    }
}

/// Pending (usable) invitations, newest first (Admin only).
#[utoipa::path(get, path = "/api/invitations", tag = "accounts", params(Pagination),
    responses(
        (status = 200, body = Page<Invitation>),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn list_invitations(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(page): Query<Pagination>,
) -> Result<Json<Page<Invitation>>, ApiError> {
    require_permission(&user, Permission::ManageAccounts)?;
    let cursor = page.cursor()?;
    let rows: Vec<InvitationRow> = sqlx::query_as(&format!(
        "SELECT {INVITATION_COLUMNS} FROM invitations
         WHERE {PENDING} AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3))
         ORDER BY created_at DESC, id DESC LIMIT $1"
    ))
    .bind(page.limit() + 1)
    .bind(cursor.map(|c| c.created_at))
    .bind(cursor.map(|c| c.id))
    .fetch_all(&state.pool)
    .await?;
    let items = rows
        .into_iter()
        .map(Invitation::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(Page::from_rows(items, page.limit(), |i| {
        crate::api::Cursor {
            created_at: i.created_at,
            id: i.id,
        }
    })))
}

/// Revokes a pending invitation (Admin only). Used or already revoked
/// invitations are `not_found`.
#[utoipa::path(post, path = "/api/invitations/{id}/revoke", tag = "accounts",
    params(("id" = Uuid, Path)),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn revoke_invitation(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require_permission(&user, Permission::ManageAccounts)?;
    let mut tx = state.pool.begin().await?;
    let revoked = sqlx::query(
        "UPDATE invitations SET revoked_at = now()
         WHERE id = $1 AND accepted_at IS NULL AND revoked_at IS NULL",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if revoked == 0 {
        return Err(ApiError::NotFound);
    }
    let metadata = json!({ "invitation_id": id });
    audit(
        &mut *tx,
        "invitation_revoked",
        Some(user.account_id()),
        None,
        metadata,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TokenRequest {
    pub token: String,
}

/// What an invitee sees before accepting. Opening a link never consumes it.
#[derive(Serialize, ToSchema)]
pub struct InvitationPreview {
    pub role: Role,
    #[schema(required = true)]
    pub email: Option<String>,
    pub language: Language,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: OffsetDateTime,
}

/// Checks an invitation token without consuming it. The token travels in the
/// body, never in the URL, so it stays out of request logs.
#[utoipa::path(post, path = "/api/invitations/lookup", tag = "accounts", request_body = TokenRequest,
    responses(
        (status = 200, body = InvitationPreview),
        (status = 410, description = "`invitation_invalid`", body = crate::api::ErrorEnvelope),
        (status = 429, description = "`rate_limited`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn lookup_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    client: Client,
    Json(body): Json<TokenRequest>,
) -> Result<Json<InvitationPreview>, ApiError> {
    auth.limit_ip("invitation", &client)?;
    let row: InvitationRow = sqlx::query_as(&format!(
        "SELECT {INVITATION_COLUMNS} FROM invitations WHERE token_hash = $1 AND {PENDING}"
    ))
    .bind(token_hash(&body.token))
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::InvitationInvalid)?;
    let i = Invitation::try_from(row)?;
    Ok(Json(InvitationPreview {
        role: i.role,
        email: i.email,
        language: i.language,
        expires_at: i.expires_at,
    }))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AcceptInvitationRequest {
    pub token: String,
    /// 3-64 characters: letters, digits, `.`, `_`, `-`. Case-insensitive.
    pub username: String,
    pub email: String,
    /// 12-256 characters.
    pub password: String,
    pub language: Language,
}

/// Creates the invited account with the invitation's role and signs it in.
/// The invitation is consumed only when this commits; validation errors and
/// username collisions leave it usable. Concurrent acceptance has one winner.
#[utoipa::path(post, path = "/api/invitations/accept", tag = "accounts", request_body = AcceptInvitationRequest,
    responses(
        (status = 200, description = "Account created; session cookie set", body = Account),
        (status = 410, description = "`invitation_invalid`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`username: taken|invalid|...`, `email`, `password`)", body = crate::api::ErrorEnvelope),
        (status = 429, description = "`rate_limited`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn accept_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    client: Client,
    headers: HeaderMap,
    Json(body): Json<AcceptInvitationRequest>,
) -> Result<Response, ApiError> {
    let username = body.username.trim().to_owned();
    let normalized = username.to_lowercase();
    auth.limit_attempt("invite", &client, &normalized)?;
    let mut errors = FieldErrors::new();
    if let Err(code) = check_username(&normalized) {
        errors.insert("username".into(), vec![code]);
    }
    if !valid_email(&body.email) {
        errors.insert("email".into(), vec!["invalid"]);
    }
    if let Err(code) = check_password(&body.password, &normalized) {
        errors.insert("password".into(), vec![code]);
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let password = body.password;
    let hash = blocking(move || hash_password(&password)).await?;

    let mut tx = state.pool.begin().await?;
    // The row lock makes a concurrent acceptance wait, then re-check and miss.
    let (invitation_id, role): (Uuid, String) = sqlx::query_as(&format!(
        "SELECT id, role::text FROM invitations WHERE token_hash = $1 AND {PENDING} FOR UPDATE"
    ))
    .bind(token_hash(&body.token))
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::InvitationInvalid)?;
    let account_id: Uuid = sqlx::query_scalar(
        "INSERT INTO accounts (username, display_name, email, password_hash, role, language)
         VALUES ($1, $1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(&username)
    .bind(body.email.trim())
    .bind(&hash)
    .bind(&role)
    .bind(ident_str(body.language))
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| unique_violation(e, "username"))?;
    sqlx::query("UPDATE invitations SET accepted_at = now(), account_id = $2 WHERE id = $1")
        .bind(invitation_id)
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    let id = Some(account_id);
    let metadata = json!({ "invitation_id": invitation_id, "role": role });
    audit(&mut *tx, "invitation_accepted", id, id, metadata).await?;
    end_presented_session(&mut tx, &headers).await?;
    let token = create_session(&mut tx, &auth, account_id, &client).await?;
    let account = load_account(&mut *tx, account_id).await?;
    tx.commit().await?;
    Ok(signed_in(&auth, &token, account))
}

// ------------------------------------------------------------------- recovery

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRequest {
    pub username: String,
}

/// Starts password recovery for a username. Always `202` with no body, whether
/// or not the account exists, so the response reveals nothing. An active
/// account gets a one-hour, single-use link at its stored email when SMTP is configured.
#[utoipa::path(post, path = "/api/auth/recovery", tag = "auth", request_body = RecoveryRequest,
    responses(
        (status = 202, description = "Accepted; identical for unknown usernames"),
        (status = 429, description = "`rate_limited`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn request_recovery(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    client: Client,
    Json(body): Json<RecoveryRequest>,
) -> Result<StatusCode, ApiError> {
    let normalized: String = body
        .username
        .trim()
        .to_lowercase()
        .chars()
        .take(64)
        .collect();
    auth.limit_attempt("recovery", &client, &normalized)?;
    // By username only: several accounts may share an email.
    let found: Option<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT id, username, email, language FROM accounts
         WHERE username_normalized = $1 AND is_active",
    )
    .bind(&normalized)
    .fetch_optional(&state.pool)
    .await?;
    let mut tx = state.pool.begin().await?;
    let token = new_token();
    // Runs for unknown usernames too (nil ID), so both paths do the same work.
    // ponytail: two simultaneous requests can both pass; the rate limit bounds that.
    let cooling: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM password_resets WHERE account_id = $1 AND used_at IS NULL
           AND expires_at > now() AND created_at > now() - {RESET_COOLDOWN})"
    ))
    .bind(found.as_ref().map_or(Uuid::nil(), |f| f.0))
    .fetch_one(&mut *tx)
    .await?;
    let found = found.filter(|_| !cooling);
    if let Some((id, ..)) = &found {
        sqlx::query(&format!(
            "INSERT INTO password_resets (account_id, token_hash, expires_at)
             VALUES ($1, $2, now() + {RESET_TTL})"
        ))
        .bind(id)
        .bind(token_hash(&token))
        .execute(&mut *tx)
        .await?;
    }
    let subject = found.as_ref().map(|f| f.0);
    let metadata =
        json!({ "ip": client.ip, "matched": found.is_some() || cooling, "cooldown": cooling });
    audit(&mut *tx, "recovery_requested", None, subject, metadata).await?;
    tx.commit().await?;

    if let Some((_, username, email, language)) = found {
        let language: Language = ident(language)?;
        let url = format!(
            "{}/reset-password/{token}?lang={}",
            auth.settings.public_origin,
            ident_str(language)
        );
        let (subject, text) = recovery_email(language, &username, &url);
        if !auth.settings.mailer.send(&email, subject, text) {
            // Never log the token or link.
            tracing::info!(
                "password recovery requested, but no email was sent (SMTP not configured)"
            );
        }
    }
    Ok(StatusCode::ACCEPTED)
}

fn recovery_email(language: Language, username: &str, url: &str) -> (&'static str, String) {
    match language {
        Language::En => (
            "Reset your Tikkit password",
            format!(
                "Someone asked to reset the password of the Tikkit account \"{username}\".\n\n\
                 Open this link to choose a new password:\n{url}\n\n\
                 The link works once and expires in one hour. If you did not ask for this, \
                 ignore this email; your password stays unchanged."
            ),
        ),
        Language::De => (
            "Tikkit-Passwort zurücksetzen",
            format!(
                "Für das Tikkit-Konto „{username}“ wurde das Zurücksetzen des Passworts angefordert.\n\n\
                 Öffnen Sie diesen Link, um ein neues Passwort festzulegen:\n{url}\n\n\
                 Der Link funktioniert einmal und ist eine Stunde gültig. Wenn Sie das nicht \
                 angefordert haben, ignorieren Sie diese E-Mail; Ihr Passwort bleibt unverändert."
            ),
        ),
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResetPasswordRequest {
    pub token: String,
    /// 12-256 characters.
    pub new_password: String,
}

/// Sets a new password with a recovery token, invalidates every other recovery
/// token of the account, and revokes all its sessions. Sign in afterwards.
#[utoipa::path(post, path = "/api/auth/recovery/reset", tag = "auth", request_body = ResetPasswordRequest,
    responses(
        (status = 204, description = "Password changed; all sessions revoked"),
        (status = 410, description = "`reset_token_invalid`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`new_password`)", body = crate::api::ErrorEnvelope),
        (status = 429, description = "`rate_limited`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn reset_password(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    client: Client,
    Json(body): Json<ResetPasswordRequest>,
) -> Result<StatusCode, ApiError> {
    auth.limit_ip("reset", &client)?;
    let mut tx = state.pool.begin().await?;
    let (account_id, username): (Uuid, String) = sqlx::query_as(
        "SELECT a.id, a.username FROM password_resets r JOIN accounts a ON a.id = r.account_id
         WHERE r.token_hash = $1 AND r.used_at IS NULL AND r.expires_at > now() AND a.is_active
         FOR UPDATE OF r",
    )
    .bind(token_hash(&body.token))
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::ResetTokenInvalid)?;
    // Rejected here, the transaction rolls back and the token stays usable.
    check_password(&body.new_password, &username.trim().to_lowercase())
        .map_err(|code| field_error("new_password", code))?;
    let password = body.new_password;
    let hash = blocking(move || hash_password(&password)).await?;
    sqlx::query("UPDATE accounts SET password_hash = $2 WHERE id = $1")
        .bind(account_id)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE password_resets SET used_at = now() WHERE account_id = $1 AND used_at IS NULL",
    )
    .bind(account_id)
    .execute(&mut *tx)
    .await?;
    let revoked = revoke_all_sessions(&mut *tx, account_id).await?;
    let id = Some(account_id);
    audit(&mut *tx, "recovery_completed", id, id, json!({})).await?;
    let metadata = json!({ "reason": "password_reset", "count": revoked });
    audit(&mut *tx, "session_revoked", id, id, metadata).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// -------------------------------------------------------------------- profile

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateProfileRequest {
    /// Contact address; omitted keeps the current one.
    pub email: Option<String>,
    /// Interface and email language; omitted keeps the current one.
    pub language: Option<Language>,
}

/// Updates the signed-in account's email and/or language.
#[utoipa::path(patch, path = "/api/auth/me", tag = "auth", request_body = UpdateProfileRequest,
    responses(
        (status = 200, body = Account),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`email: invalid`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn update_profile(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<UpdateProfileRequest>,
) -> Result<Json<Account>, ApiError> {
    let email = body.email.map(|e| e.trim().to_owned());
    if email.as_deref().is_some_and(|e| !valid_email(e)) {
        return Err(field_error("email", "invalid"));
    }
    let language = body.language.map(ident_str);
    let fields: Vec<&str> = [("email", email.is_some()), ("language", language.is_some())]
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect();
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE accounts SET email = coalesce($2, email), language = coalesce($3, language)
         WHERE id = $1",
    )
    .bind(user.account_id())
    .bind(&email)
    .bind(&language)
    .execute(&mut *tx)
    .await?;
    let id = Some(user.account_id());
    audit(
        &mut *tx,
        "profile_updated",
        id,
        id,
        json!({ "fields": fields }),
    )
    .await?;
    let account = load_account(&mut *tx, user.account_id()).await?;
    tx.commit().await?;
    Ok(Json(account))
}

// ---------------------------------------------------------- Admin: accounts

#[derive(Serialize, ToSchema)]
pub struct AccountSummary {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub email: String,
    pub role: Role,
    pub language: Language,
    pub is_active: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

/// All accounts, newest first (Admin only).
#[utoipa::path(get, path = "/api/accounts", tag = "accounts", params(Pagination),
    responses(
        (status = 200, body = Page<AccountSummary>),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn list_accounts(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(page): Query<Pagination>,
) -> Result<Json<Page<AccountSummary>>, ApiError> {
    require_permission(&user, Permission::ManageAccounts)?;
    let cursor = page.cursor()?;
    type Row = (
        Uuid,
        String,
        String,
        String,
        String,
        String,
        bool,
        OffsetDateTime,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, username, display_name, email, role::text, language, is_active, created_at
         FROM accounts WHERE ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3))
         ORDER BY created_at DESC, id DESC LIMIT $1",
    )
    .bind(page.limit() + 1)
    .bind(cursor.map(|c| c.created_at))
    .bind(cursor.map(|c| c.id))
    .fetch_all(&state.pool)
    .await?;
    let items = rows
        .into_iter()
        .map(|r| {
            Ok(AccountSummary {
                id: r.0,
                username: r.1,
                display_name: r.2,
                email: r.3,
                role: ident(r.4)?,
                language: ident(r.5)?,
                is_active: r.6,
                created_at: r.7,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(Json(Page::from_rows(items, page.limit(), |a| {
        crate::api::Cursor {
            created_at: a.created_at,
            id: a.id,
        }
    })))
}

/// Deactivates an account (Admin only): login is refused and all its sessions
/// end immediately. Tickets and audit history are kept. The last active Admin
/// cannot be deactivated.
#[utoipa::path(post, path = "/api/accounts/{id}/deactivate", tag = "accounts",
    params(("id" = Uuid, Path)),
    responses(
        (status = 204, description = "Deactivated (or already inactive)"),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope),
        (status = 409, description = "`last_active_admin`, `active_team_lead`, or `conflict` (concurrent change)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn deactivate_account(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    set_active(&state, &user, id, false).await
}

/// Reactivates an account (Admin only).
#[utoipa::path(post, path = "/api/accounts/{id}/reactivate", tag = "accounts",
    params(("id" = Uuid, Path)),
    responses(
        (status = 204, description = "Reactivated (or already active)"),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn reactivate_account(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    set_active(&state, &user, id, true).await
}

async fn set_active(
    state: &AppState,
    user: &CurrentUser,
    id: Uuid,
    active: bool,
) -> Result<StatusCode, ApiError> {
    require_permission(user, Permission::ManageAccounts)?;
    let mut tx = state.pool.begin().await?;
    // The last-admin and team-lead rules are database constraints that `?` maps
    // to `last_active_admin` / `active_team_lead`. Do not pre-lock Admin rows here.
    let changed =
        sqlx::query("UPDATE accounts SET is_active = $2 WHERE id = $1 AND is_active <> $2")
            .bind(id)
            .bind(active)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if changed == 0 {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM accounts WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        return if exists {
            Ok(StatusCode::NO_CONTENT)
        } else {
            Err(ApiError::NotFound)
        };
    }
    let actor = Some(user.account_id());
    if active {
        audit(&mut *tx, "account_reactivated", actor, Some(id), json!({})).await?;
    } else {
        let revoked = revoke_all_sessions(&mut *tx, id).await?;
        // Its recovery links and, for an Admin, the invitations it issued die with it.
        sqlx::query(
            "UPDATE password_resets SET used_at = now() WHERE account_id = $1 AND used_at IS NULL",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let invitations = sqlx::query(
            "UPDATE invitations SET revoked_at = now()
             WHERE created_by = $1 AND accepted_at IS NULL AND revoked_at IS NULL",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        audit(&mut *tx, "account_deactivated", actor, Some(id), json!({})).await?;
        let metadata = json!({ "reason": "account_deactivated", "count": revoked });
        audit(&mut *tx, "session_revoked", actor, Some(id), metadata).await?;
        if invitations > 0 {
            let metadata = json!({ "reason": "issuer_deactivated", "count": invitations });
            audit(&mut *tx, "invitation_revoked", actor, Some(id), metadata).await?;
        }
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

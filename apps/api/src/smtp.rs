//! Admin outbound SMTP settings. See docs/schema.md and docs/auth.md.
//!
//! Ticket notification templates and the outbox belong to #27. This module only
//! stores the connection and exposes the test action.

use std::sync::Arc;

use axum::{Extension, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::{
    AppState,
    api::{ApiError, FieldErrors, Json},
    auth::{Auth, CurrentUser, audit, ident, valid_email},
    authz::{Permission, require_permission},
    mail::{self, DeliverOptions, MailError, OutboundMessage, SmtpSecurity},
};

#[derive(Serialize, ToSchema)]
pub struct SmtpSettings {
    #[schema(required = true)]
    pub connection: Option<SmtpConnection>,
}

/// Masked connection. The username and password are never included.
#[derive(Serialize, ToSchema)]
pub struct SmtpConnection {
    pub host: String,
    pub port: u16,
    pub security: SmtpSecurity,
    pub sender_name: String,
    pub sender_address: String,
    /// Always true when a connection exists: a username and password are stored.
    /// Neither value is returned.
    pub password_set: bool,
    pub enabled: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
    /// Stable code from the last background delivery failure, if any.
    #[schema(required = true)]
    pub last_delivery_error: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub last_delivery_error_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub last_test_at: Option<OffsetDateTime>,
    #[schema(required = true)]
    pub last_test_error: Option<String>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SmtpSettingsWrite {
    pub host: String,
    pub port: u16,
    pub security: SmtpSecurity,
    pub sender_name: String,
    pub sender_address: String,
    /// Required when creating the connection or changing the host, so stored
    /// credentials are never sent to a new server. Omit to keep the stored value.
    #[serde(default)]
    pub username: Option<String>,
    /// Same rule as `username`.
    #[serde(default)]
    pub password: Option<String>,
    pub enabled: bool,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SmtpTestRequest {
    /// Recipient. Must be the acting Admin's email or the configured sender address.
    pub to: String,
}

#[derive(Serialize, ToSchema)]
pub struct SmtpTestResult {
    pub delivered: bool,
}

#[derive(sqlx::FromRow)]
struct ConnectionRow {
    host: String,
    port: i32,
    security: String,
    sender_name: String,
    sender_address: String,
    enabled: bool,
    updated_at: OffsetDateTime,
    last_delivery_error: Option<String>,
    last_delivery_error_at: Option<OffsetDateTime>,
    last_test_at: Option<OffsetDateTime>,
    last_test_error: Option<String>,
}

const COLUMNS: &str = "host, port, security, sender_name, sender_address, enabled,
    updated_at, last_delivery_error, last_delivery_error_at, last_test_at, last_test_error";

fn view(row: ConnectionRow) -> Result<SmtpConnection, ApiError> {
    let security = match row.security.as_str() {
        "starttls" => SmtpSecurity::StartTls,
        "implicit_tls" => SmtpSecurity::ImplicitTls,
        _ => return Err(ApiError::Internal),
    };
    let port = u16::try_from(row.port).map_err(|_| ApiError::Internal)?;
    Ok(SmtpConnection {
        host: row.host,
        port,
        security,
        sender_name: row.sender_name,
        sender_address: row.sender_address,
        password_set: true,
        enabled: row.enabled,
        updated_at: row.updated_at,
        last_delivery_error: row.last_delivery_error,
        last_delivery_error_at: row.last_delivery_error_at,
        last_test_at: row.last_test_at,
        last_test_error: row.last_test_error,
    })
}

fn mail_api(error: MailError) -> ApiError {
    match error {
        MailError::NotConfigured | MailError::Disabled => ApiError::SmtpNotConfigured,
        MailError::Auth => ApiError::SmtpAuthFailed,
        MailError::Tls => ApiError::SmtpTlsFailed,
        MailError::Network => ApiError::SmtpNetworkFailed,
        MailError::Delivery => ApiError::SmtpDeliveryFailed,
        MailError::Unreadable => ApiError::SmtpCredentialUnreadable,
        MailError::Store => ApiError::Internal,
    }
}

/// The saved outbound connection, or `connection: null` (Admin only).
#[utoipa::path(get, path = "/api/settings/smtp", tag = "settings",
    responses(
        (status = 200, body = SmtpSettings),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn get_smtp(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<SmtpSettings>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let row: Option<ConnectionRow> =
        sqlx::query_as(&format!("SELECT {COLUMNS} FROM smtp_connection"))
            .fetch_optional(&state.pool)
            .await?;
    Ok(Json(SmtpSettings {
        connection: row.map(view).transpose()?,
    }))
}

/// Creates or replaces the outbound connection (Admin only).
/// A missing `password` keeps the stored one. The response never contains it.
#[utoipa::path(put, path = "/api/settings/smtp", tag = "settings", request_body = SmtpSettingsWrite,
    responses(
        (status = 200, body = SmtpSettings),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn put_smtp(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    user: CurrentUser,
    Json(body): Json<SmtpSettingsWrite>,
) -> Result<Json<SmtpSettings>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let mut tx = state.pool.begin().await?;
    let existing: Option<(String, Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT host, username_ciphertext, password_ciphertext FROM smtp_connection FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let write = validate(&body, existing.as_ref().map(|(host, ..)| host.as_str()))?;
    let created = existing.is_none();
    let key = &auth.settings.encryption_key;
    // validate() requires both credentials unless a connection to the same host exists.
    let (username, password) = match (&write.username, &body.password, existing) {
        (Some(u), Some(p), _) => (mail::seal(key, u), mail::seal(key, p)),
        (u, p, Some((_, stored_u, stored_p))) => (
            u.as_deref().map_or(stored_u, |u| mail::seal(key, u)),
            p.as_deref().map_or(stored_p, |p| mail::seal(key, p)),
        ),
        _ => return Err(ApiError::Internal),
    };
    let row: ConnectionRow = sqlx::query_as(&format!(
        "INSERT INTO smtp_connection
            (host, port, security, sender_name, sender_address, username_ciphertext,
             password_ciphertext, enabled, updated_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (id) DO UPDATE SET
            host = EXCLUDED.host,
            port = EXCLUDED.port,
            security = EXCLUDED.security,
            sender_name = EXCLUDED.sender_name,
            sender_address = EXCLUDED.sender_address,
            username_ciphertext = EXCLUDED.username_ciphertext,
            password_ciphertext = EXCLUDED.password_ciphertext,
            enabled = EXCLUDED.enabled,
            updated_by = EXCLUDED.updated_by,
            updated_at = now()
         RETURNING {COLUMNS}"
    ))
    .bind(&write.host)
    .bind(i32::from(write.port))
    .bind(security_name(write.security))
    .bind(&write.sender_name)
    .bind(&write.sender_address)
    .bind(&username)
    .bind(&password)
    .bind(write.enabled)
    .bind(user.account_id())
    .fetch_one(&mut *tx)
    .await?;
    audit(
        &mut *tx,
        if created {
            "smtp_configured"
        } else {
            "smtp_updated"
        },
        Some(user.account_id()),
        None,
        json!({
            "host": write.host,
            "port": write.port,
            "security": security_name(write.security),
            "sender_address": write.sender_address,
            "enabled": write.enabled,
            "username_changed": write.username.is_some(),
            "password_changed": body.password.is_some(),
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(SmtpSettings {
        connection: Some(view(row)?),
    }))
}

/// Removes the outbound connection (Admin only). Later mail is not sent.
#[utoipa::path(delete, path = "/api/settings/smtp", tag = "settings",
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn delete_smtp(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<StatusCode, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let mut tx = state.pool.begin().await?;
    let removed: Option<(String, i32, String)> =
        sqlx::query_as("DELETE FROM smtp_connection RETURNING host, port, security")
            .fetch_optional(&mut *tx)
            .await?;
    let Some((host, port, security)) = removed else {
        return Err(ApiError::NotFound);
    };
    audit(
        &mut *tx,
        "smtp_removed",
        Some(user.account_id()),
        None,
        json!({ "host": host, "port": port, "security": security }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Sends a test message through the saved connection (Admin only).
/// The recipient must be this Admin's email or the configured sender address.
/// Transport security is the saved STARTTLS or implicit TLS setting; it is not
/// taken from the request. Failures use stable codes and do not echo credentials.
#[utoipa::path(post, path = "/api/settings/smtp/test", tag = "settings",
    request_body = SmtpTestRequest,
    responses(
        (status = 200, body = SmtpTestResult),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 409, description = "`smtp_not_configured`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`to`: `invalid` or `not_allowed`)", body = crate::api::ErrorEnvelope),
        (status = 502, description = "`smtp_auth_failed`, `smtp_tls_failed`, `smtp_network_failed`, `smtp_delivery_failed`, or `smtp_credential_unreadable`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn test_smtp(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    user: CurrentUser,
    Json(body): Json<SmtpTestRequest>,
) -> Result<Json<SmtpTestResult>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let to = body.to.trim();
    if !valid_email(to) {
        return Err(field("to", "invalid"));
    }
    let (account_email, language): (String, String) =
        sqlx::query_as("SELECT email, language FROM accounts WHERE id = $1")
            .bind(user.account_id())
            .fetch_one(&state.pool)
            .await?;
    let sender: Option<String> = sqlx::query_scalar("SELECT sender_address FROM smtp_connection")
        .fetch_optional(&state.pool)
        .await?;
    let Some(sender) = sender else {
        return Err(ApiError::SmtpNotConfigured);
    };
    if !to.eq_ignore_ascii_case(&account_email) && !to.eq_ignore_ascii_case(&sender) {
        return Err(field("to", "not_allowed"));
    }
    let language = ident(language)?;
    let (subject, text) = test_message(language, &auth.settings.public_origin);
    // One attempt so a dead relay answers within SMTP_TIMEOUT, not three times that.
    let result = mail::deliver(
        &state.pool,
        &auth.settings.encryption_key,
        auth.settings.smtp_trust_root_pem.as_deref(),
        &OutboundMessage {
            to: to.to_owned(),
            subject: subject.to_owned(),
            body: text,
            idempotency_key: None,
        },
        DeliverOptions {
            allow_disabled: true,
            max_attempts: 1,
            ..DeliverOptions::default()
        },
    )
    .await;
    let code = result.as_ref().err().map(|error| error.code());
    sqlx::query("UPDATE smtp_connection SET last_test_at = now(), last_test_error = $1")
        .bind(code)
        .execute(&state.pool)
        .await?;
    audit(
        &state.pool,
        "smtp_tested",
        Some(user.account_id()),
        None,
        json!({ "result": code.unwrap_or("ok") }),
    )
    .await?;
    result.map_err(mail_api)?;
    Ok(Json(SmtpTestResult { delivered: true }))
}

fn test_message(language: crate::api::Language, origin: &str) -> (&'static str, String) {
    match language {
        crate::api::Language::En => (
            "Tikkit email test",
            format!(
                "This is a test message from Tikkit. Outbound email is working.\n\n\
                 Create and reply to tickets in the portal:\n{origin}\n\n\
                 Do not reply to this email."
            ),
        ),
        crate::api::Language::De => (
            "Tikkit-E-Mail-Test",
            format!(
                "Dies ist eine Testnachricht von Tikkit. Ausgehende E-Mail funktioniert.\n\n\
                 Tickets werden im Portal erstellt und beantwortet:\n{origin}\n\n\
                 Antworten Sie nicht auf diese E-Mail."
            ),
        ),
    }
}

struct ValidWrite {
    host: String,
    port: u16,
    security: SmtpSecurity,
    sender_name: String,
    sender_address: String,
    username: Option<String>,
    enabled: bool,
}

/// `stored_host` is the saved connection's host, if any. Credentials are
/// required unless they would go to that same host.
fn validate(body: &SmtpSettingsWrite, stored_host: Option<&str>) -> Result<ValidWrite, ApiError> {
    let host = body.host.trim().trim_end_matches('.').to_owned();
    let sender_name = body.sender_name.trim().to_owned();
    let sender_address = body.sender_address.trim().to_owned();
    let username = body.username.as_deref().map(|u| u.trim().to_owned());
    let keep = stored_host.is_some_and(|stored| stored.eq_ignore_ascii_case(&host));
    let mut errors = FieldErrors::new();
    if !valid_host(&host) {
        errors.insert("host".into(), vec!["invalid"]);
    }
    if body.port == 0 {
        errors.insert("port".into(), vec!["invalid"]);
    }
    if !valid_sender_name(&sender_name) {
        errors.insert("sender_name".into(), vec!["invalid"]);
    }
    if !valid_email(&sender_address) {
        errors.insert("sender_address".into(), vec!["invalid"]);
    }
    match &username {
        None if !keep => {
            errors.insert("username".into(), vec!["required"]);
        }
        Some(u) if u.is_empty() || u.chars().count() > 256 || u.chars().any(char::is_control) => {
            errors.insert("username".into(), vec!["invalid"]);
        }
        _ => {}
    }
    match &body.password {
        None if !keep => {
            errors.insert("password".into(), vec!["required"]);
        }
        Some(password) if !valid_password(password) => {
            let code = if password.chars().count() > 1024 {
                "too_long"
            } else {
                "invalid"
            };
            errors.insert("password".into(), vec![code]);
        }
        _ => {}
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    Ok(ValidWrite {
        host,
        port: body.port,
        security: body.security,
        sender_name,
        sender_address,
        username,
        enabled: body.enabled,
    })
}

fn valid_password(password: &str) -> bool {
    let len = password.chars().count();
    (1..=1024).contains(&len) && !password.chars().any(|c| c.is_control())
}

fn valid_sender_name(name: &str) -> bool {
    let len = name.chars().count();
    (1..=128).contains(&len)
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '\\'))
}

pub(crate) fn valid_host(host: &str) -> bool {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    host.split('.').all(|label| {
        (1..=63).contains(&label.len())
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    })
}

fn security_name(security: SmtpSecurity) -> &'static str {
    match security {
        SmtpSecurity::StartTls => "starttls",
        SmtpSecurity::ImplicitTls => "implicit_tls",
    }
}

fn field(name: &str, code: &'static str) -> ApiError {
    ApiError::Validation(FieldErrors::from([(name.into(), vec![code])]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_dns_names_or_addresses() {
        assert!(valid_host("smtp.example.com"));
        assert!(valid_host("127.0.0.1"));
        assert!(valid_host("::1"));
        assert!(!valid_host(""));
        assert!(!valid_host("smtp.example.com/path"));
        assert!(!valid_host("-bad.example"));
        assert!(!valid_host("has space.example"));
        assert!(!valid_host("user:pass@smtp.example.com"));
    }
}

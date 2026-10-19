//! Outbound email through the Admin-configured SMTP connection.
//!
//! Nothing is sent until an Admin saves an enabled connection. Invitation and
//! recovery mail is queued after the database commit, so a delivery failure
//! never rolls back portal work. The notification outbox (#27) should call
//! [`deliver`] with its own idempotency key; this module does not depend on it.
//!
//! Credentials stay in memory only for the send. They are never written to
//! logs, errors, or the idempotency table.

use std::{fmt, time::Duration};

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use argon2::password_hash::rand_core::{OsRng, RngCore};
use lettre::{
    Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, header::ContentType},
    transport::smtp::{
        authentication::Credentials,
        client::{Certificate, CertificateStore, Tls, TlsParameters},
    },
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;

/// Per-attempt command and connect timeout.
pub const SMTP_TIMEOUT: Duration = Duration::from_secs(15);
/// Transient network failures and 4xx replies are tried at most this many times.
pub const SMTP_ATTEMPTS: u32 = 3;

/// How the relay expects the connection to be encrypted. There is no plaintext
/// or opportunistic mode: certificate verification stays on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum SmtpSecurity {
    /// Connect in cleartext, then require STARTTLS before any credentials.
    #[serde(rename = "starttls")]
    StartTls,
    /// TLS from the first byte (SMTPS, usually port 465).
    #[serde(rename = "implicit_tls")]
    ImplicitTls,
}

#[derive(Clone, Copy, Debug)]
pub struct DeliverOptions {
    pub timeout: Duration,
    pub max_attempts: u32,
    /// The Admin test action may try a connection that is saved but disabled.
    pub allow_disabled: bool,
}

impl Default for DeliverOptions {
    fn default() -> Self {
        Self {
            timeout: SMTP_TIMEOUT,
            max_attempts: SMTP_ATTEMPTS,
            allow_disabled: false,
        }
    }
}

pub struct OutboundMessage {
    pub to: String,
    pub subject: String,
    pub body: String,
    /// When set, a successful send is recorded and a later call does not send again.
    pub idempotency_key: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Delivered {
    Sent,
    /// This key was already delivered. Nothing was transmitted.
    Duplicate,
}

/// Safe delivery failure. `Display` is the stable code and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailError {
    NotConfigured,
    Disabled,
    Auth,
    Tls,
    Network,
    Delivery,
    /// The stored password could not be decrypted with the current key.
    Unreadable,
    /// The database could not be read or updated. Details stay in the server log.
    Store,
}

impl MailError {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotConfigured => "smtp_not_configured",
            Self::Disabled => "smtp_disabled",
            Self::Auth => "smtp_auth_failed",
            Self::Tls => "smtp_tls_failed",
            Self::Network => "smtp_network_failed",
            Self::Delivery => "smtp_delivery_failed",
            Self::Unreadable => "smtp_credential_unreadable",
            Self::Store => "internal",
        }
    }
}

impl fmt::Display for MailError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// Decrypted connection. `Debug` redacts the password.
pub(crate) struct ReadySmtp {
    pub host: String,
    pub port: u16,
    pub security: SmtpSecurity,
    pub sender_name: String,
    pub sender_address: String,
    pub username: String,
    pub password: String,
    pub enabled: bool,
}

impl fmt::Debug for ReadySmtp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadySmtp")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("security", &self.security)
            .field("sender_address", &self.sender_address)
            .field("username", &"<redacted>")
            .field("password", &"<redacted>")
            .field("enabled", &self.enabled)
            .finish()
    }
}

/// AES-256-GCM. Layout: 12-byte nonce || ciphertext and tag.
pub(crate) fn seal(key: &[u8; 32], plaintext: &str) -> Vec<u8> {
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .expect("aes-256-gcm encrypt");
    let mut out = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    out
}

fn open(key: &[u8; 32], packed: &[u8]) -> Option<String> {
    if packed.len() < 12 + 16 {
        return None;
    }
    let (nonce, ciphertext) = packed.split_at(12);
    let cipher = Aes256Gcm::new(key.into());
    let plain = cipher.decrypt(Nonce::from_slice(nonce), ciphertext).ok()?;
    String::from_utf8(plain).ok()
}

#[derive(sqlx::FromRow)]
struct SecretRow {
    host: String,
    port: i32,
    security: String,
    sender_name: String,
    sender_address: String,
    username_ciphertext: Vec<u8>,
    password_ciphertext: Vec<u8>,
    enabled: bool,
}

pub(crate) async fn load(pool: &PgPool, key: &[u8; 32]) -> Result<ReadySmtp, MailError> {
    let row: Option<SecretRow> = sqlx::query_as(
        "SELECT host, port, security, sender_name, sender_address, username_ciphertext,
                password_ciphertext, enabled
         FROM smtp_connection",
    )
    .fetch_optional(pool)
    .await
    .map_err(|_| MailError::Store)?;
    let Some(row) = row else {
        return Err(MailError::NotConfigured);
    };
    let username = open(key, &row.username_ciphertext).ok_or(MailError::Unreadable)?;
    let password = open(key, &row.password_ciphertext).ok_or(MailError::Unreadable)?;
    let security = match row.security.as_str() {
        "starttls" => SmtpSecurity::StartTls,
        "implicit_tls" => SmtpSecurity::ImplicitTls,
        _ => return Err(MailError::Tls),
    };
    let port = u16::try_from(row.port).map_err(|_| MailError::Tls)?;
    Ok(ReadySmtp {
        host: row.host,
        port,
        security,
        sender_name: row.sender_name,
        sender_address: row.sender_address,
        username,
        password,
        enabled: row.enabled,
    })
}

/// Queues a message when an enabled connection exists. Returns whether a send
/// was started. The outcome is not waited on; failures are logged by code only
/// and stored on the connection for the Admin screen.
pub(crate) async fn queue(
    pool: PgPool,
    encryption_key: [u8; 32],
    trust_root_pem: Option<Vec<u8>>,
    message: OutboundMessage,
) -> bool {
    match load(&pool, &encryption_key).await {
        Ok(ready) if ready.enabled => {
            tokio::spawn(async move {
                match deliver(
                    &pool,
                    &encryption_key,
                    trust_root_pem.as_deref(),
                    &message,
                    DeliverOptions::default(),
                )
                .await
                {
                    Ok(Delivered::Sent) => {
                        let _ = clear_delivery_error(&pool).await;
                    }
                    Ok(Delivered::Duplicate) => {}
                    Err(
                        failure @ (MailError::Auth
                        | MailError::Tls
                        | MailError::Network
                        | MailError::Delivery
                        | MailError::Unreadable),
                    ) => {
                        tracing::warn!(error = failure.code(), "email delivery failed");
                        let _ = note_delivery_error(&pool, failure.code()).await;
                    }
                    Err(_) => tracing::warn!("email delivery did not complete"),
                }
            });
            true
        }
        Ok(_) | Err(MailError::NotConfigured | MailError::Disabled) => false,
        Err(MailError::Unreadable) => {
            tracing::warn!("email was not queued: stored smtp credential could not be read");
            false
        }
        Err(_) => {
            tracing::warn!("email was not queued");
            false
        }
    }
}

/// Sends one message through the saved connection.
///
/// `trust_root_pem`, when set, is the only trust anchor and certificates are
/// still verified against it. Production passes `None`, which uses the default
/// WebPKI roots. Verification is never disabled.
///
/// A transient failure is retried at most [`SMTP_ATTEMPTS`] times. Each attempt
/// is bounded by `options.timeout` (DNS, connect, and commands). Auth and TLS
/// failures are not retried. The idempotency key is recorded only after the
/// server accepts the message.
pub async fn deliver(
    pool: &PgPool,
    encryption_key: &[u8; 32],
    trust_root_pem: Option<&[u8]>,
    message: &OutboundMessage,
    options: DeliverOptions,
) -> Result<Delivered, MailError> {
    let ready = load(pool, encryption_key).await?;
    if !ready.enabled && !options.allow_disabled {
        return Err(MailError::Disabled);
    }
    if let Some(key) = &message.idempotency_key
        && already_sent(pool, key).await?
    {
        return Ok(Delivered::Duplicate);
    }
    let attempts = options.max_attempts.clamp(1, 5);
    let mut last = MailError::Network;
    for attempt in 0..attempts {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(50 * u64::from(attempt))).await;
        }
        let sent = tokio::time::timeout(
            options.timeout,
            send_once(&ready, trust_root_pem, message, options.timeout),
        )
        .await;
        match sent {
            Ok(Ok(())) => {
                if let Some(key) = &message.idempotency_key {
                    record_sent(pool, key).await?;
                }
                return Ok(Delivered::Sent);
            }
            Ok(Err(Attempt::Fatal(failure))) => return Err(failure),
            Ok(Err(Attempt::Retry(failure))) => last = failure,
            Err(_elapsed) => last = MailError::Network,
        }
    }
    Err(last)
}

enum Attempt {
    Fatal(MailError),
    Retry(MailError),
}

async fn send_once(
    ready: &ReadySmtp,
    trust_root_pem: Option<&[u8]>,
    message: &OutboundMessage,
    timeout: Duration,
) -> Result<(), Attempt> {
    let email = build_message(ready, message)?;
    let transport = build_transport(ready, trust_root_pem, timeout)?;
    match transport.send(email).await {
        Ok(_) => Ok(()),
        Err(error) => Err(classify(&error)),
    }
}

fn build_message(ready: &ReadySmtp, message: &OutboundMessage) -> Result<Message, Attempt> {
    let address: Address = ready
        .sender_address
        .parse()
        .map_err(|_| Attempt::Fatal(MailError::Delivery))?;
    let from = Mailbox::new(Some(ready.sender_name.clone()), address);
    let to: Mailbox = message
        .to
        .parse()
        .map_err(|_| Attempt::Fatal(MailError::Delivery))?;
    Message::builder()
        .from(from)
        .to(to)
        .subject(&message.subject)
        .header(ContentType::TEXT_PLAIN)
        .body(message.body.clone())
        .map_err(|_| Attempt::Fatal(MailError::Delivery))
}

/// `builder_dangerous` is the only lettre entry point that accepts an explicit
/// port. TLS is always `Required` (STARTTLS) or `Wrapper` (implicit TLS).
fn build_transport(
    ready: &ReadySmtp,
    trust_root_pem: Option<&[u8]>,
    timeout: Duration,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, Attempt> {
    let tls_parameters = match trust_root_pem {
        Some(pem) => {
            let cert = Certificate::from_pem(pem).map_err(|_| Attempt::Fatal(MailError::Tls))?;
            TlsParameters::builder(ready.host.clone())
                .certificate_store(CertificateStore::None)
                .add_root_certificate(cert)
                .build()
                .map_err(|_| Attempt::Fatal(MailError::Tls))?
        }
        None => TlsParameters::new_rustls(ready.host.clone())
            .map_err(|_| Attempt::Fatal(MailError::Tls))?,
    };
    let tls = match ready.security {
        SmtpSecurity::StartTls => Tls::Required(tls_parameters),
        SmtpSecurity::ImplicitTls => Tls::Wrapper(tls_parameters),
    };
    Ok(
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(ready.host.clone())
            .port(ready.port)
            .tls(tls)
            .credentials(Credentials::new(
                ready.username.clone(),
                ready.password.clone(),
            ))
            .timeout(Some(timeout))
            .build(),
    )
}

fn classify(error: &lettre::transport::smtp::Error) -> Attempt {
    // Certificate and STARTTLS failures are connection errors, not SMTP replies,
    // and happen before AUTH, so the text cannot contain the password.
    let text = error.to_string();
    if error.is_tls()
        || text.contains("STARTTLS")
        || (error.status().is_none() && text.contains("certificate"))
    {
        return Attempt::Fatal(MailError::Tls);
    }
    if let Some(code) = error.status() {
        let number = u16::from(code);
        if matches!(number, 530 | 534 | 535 | 538) {
            return Attempt::Fatal(MailError::Auth);
        }
        if error.is_transient() {
            return Attempt::Retry(MailError::Delivery);
        }
        return Attempt::Fatal(MailError::Delivery);
    }
    if error.is_client() {
        return Attempt::Fatal(MailError::Network);
    }
    Attempt::Retry(MailError::Network)
}

async fn already_sent(pool: &PgPool, key: &str) -> Result<bool, MailError> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM smtp_deliveries WHERE idempotency_key = $1)")
        .bind(key)
        .fetch_one(pool)
        .await
        .map_err(|_| MailError::Store)
}

async fn record_sent(pool: &PgPool, key: &str) -> Result<(), MailError> {
    sqlx::query("INSERT INTO smtp_deliveries (idempotency_key) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(key)
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|_| MailError::Store)
}

pub(crate) async fn note_delivery_error(pool: &PgPool, code: &str) -> Result<(), MailError> {
    sqlx::query(
        "UPDATE smtp_connection
         SET last_delivery_error = $1, last_delivery_error_at = now()",
    )
    .bind(code)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(|_| MailError::Store)
}

async fn clear_delivery_error(pool: &PgPool) -> Result<(), MailError> {
    sqlx::query(
        "UPDATE smtp_connection
         SET last_delivery_error = NULL, last_delivery_error_at = NULL",
    )
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(|_| MailError::Store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_round_trips_and_rejects_the_wrong_key() {
        let key = [7u8; 32];
        let packed = seal(&key, "s3cret-mail-password");
        assert!(!packed.windows(8).any(|w| w == b"s3cret-m"));
        assert_eq!(open(&key, &packed).as_deref(), Some("s3cret-mail-password"));
        assert!(open(&[8u8; 32], &packed).is_none());
        assert!(open(&key, &packed[..20]).is_none());
    }

    #[test]
    fn debug_redacts_credentials() {
        let ready = ReadySmtp {
            host: "smtp.example.com".into(),
            port: 587,
            security: SmtpSecurity::StartTls,
            sender_name: "Tikkit".into(),
            sender_address: "help@example.com".into(),
            username: "mailer-user".into(),
            password: "hunter2-secret".into(),
            enabled: true,
        };
        let text = format!("{ready:?}");
        assert!(!text.contains("hunter2"));
        assert!(!text.contains("mailer-user"));
        assert!(text.contains("<redacted>"));
        assert_eq!(MailError::Auth.to_string(), "smtp_auth_failed");
    }
}

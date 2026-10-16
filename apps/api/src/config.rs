//! Validated runtime configuration (issue #8).
//!
//! Configuration comes only from environment variables. [`Config::from_source`]
//! takes the lookup function as a parameter so validation can be tested
//! without touching the process environment. Every problem is collected and
//! reported together, and no error message or `Debug` output ever contains
//! the database password or the encryption key.

use std::{fmt, net::SocketAddr, str::FromStr, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use sqlx::postgres::PgConnectOptions;
use url::Url;

/// Database used in development when `DATABASE_URL` is unset. It matches
/// `compose.dev.yml`. Production never falls back to it.
pub const DEVELOPMENT_DATABASE_URL: &str = "postgres://tikkit:tikkit@127.0.0.1:5432/tikkit";
/// Portal origin assumed in development when `PUBLIC_ORIGIN` is unset.
pub const DEVELOPMENT_PUBLIC_ORIGIN: &str = "http://localhost:3000";
pub const DEFAULT_API_ADDR: &str = "127.0.0.1:3001";
pub const DEFAULT_LOG_FILTER: &str = "info,sqlx=warn";

const MAX_CONNECTIONS_RANGE: (u64, u64) = (1, 100);
const SHUTDOWN_TIMEOUT_RANGE: (u64, u64) = (1, 300);
const STARTUP_TIMEOUT_RANGE: (u64, u64) = (0, 600);
const ENCRYPTION_KEY_BYTES: usize = 32;

/// Deployment mode, from `TIKKIT_ENV`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Environment {
    Development,
    Production,
}

impl Environment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Production => "production",
        }
    }

    pub fn is_production(self) -> bool {
        self == Self::Production
    }
}

/// A database URL whose password never appears in `Debug` or `Display`.
#[derive(Clone)]
pub struct DatabaseUrl {
    raw: String,
    redacted: String,
    password: Option<String>,
}

impl DatabaseUrl {
    /// The full URL including credentials. Only pass it to the database driver.
    pub fn expose(&self) -> &str {
        &self.raw
    }

    /// The URL with its password replaced by `***`, safe to print.
    pub fn redacted(&self) -> &str {
        &self.redacted
    }

    pub fn connect_options(&self) -> PgConnectOptions {
        // Validated in `parse_database_url`, so this cannot fail.
        PgConnectOptions::from_str(&self.raw).expect("validated database URL")
    }

    /// Removes the password from arbitrary text, such as a driver error.
    pub fn scrub(&self, text: &str) -> String {
        match &self.password {
            Some(password) if !password.is_empty() => text.replace(password.as_str(), "***"),
            _ => text.to_owned(),
        }
    }
}

impl fmt::Debug for DatabaseUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted)
    }
}

impl fmt::Display for DatabaseUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted)
    }
}

/// The 32-byte key that encrypts stored secrets (future AI/SMTP credentials).
#[derive(Clone)]
pub struct EncryptionKey([u8; ENCRYPTION_KEY_BYTES]);

impl EncryptionKey {
    pub fn expose(&self) -> &[u8; ENCRYPTION_KEY_BYTES] {
        &self.0
    }
}

impl fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncryptionKey(***)")
    }
}

/// Fully validated runtime configuration.
#[derive(Clone)]
pub struct Config {
    pub environment: Environment,
    pub database_url: DatabaseUrl,
    /// True when `DATABASE_URL` was unset and the development default is used.
    pub database_url_is_default: bool,
    pub database_max_connections: u32,
    pub database_auto_migrate: bool,
    /// How long startup keeps retrying an unreachable database.
    pub database_startup_timeout: Duration,
    pub api_addr: SocketAddr,
    /// Serialized origin such as `https://help.example.com` (no trailing slash).
    pub public_origin: String,
    pub encryption_key: Option<EncryptionKey>,
    pub shutdown_timeout: Duration,
    pub trust_proxy_headers: bool,
    pub log_filter: String,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("environment", &self.environment)
            .field("database_url", &self.database_url)
            .field("database_url_is_default", &self.database_url_is_default)
            .field("database_max_connections", &self.database_max_connections)
            .field("database_auto_migrate", &self.database_auto_migrate)
            .field("database_startup_timeout", &self.database_startup_timeout)
            .field("api_addr", &self.api_addr)
            .field("public_origin", &self.public_origin)
            .field(
                "encryption_key",
                &self.encryption_key.as_ref().map(|_| "***"),
            )
            .field("shutdown_timeout", &self.shutdown_timeout)
            .field("trust_proxy_headers", &self.trust_proxy_headers)
            .field("log_filter", &self.log_filter)
            .finish()
    }
}

/// One invalid or missing setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigProblem {
    pub variable: &'static str,
    pub message: String,
}

/// All problems found while loading configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    pub problems: Vec<ConfigProblem>,
}

impl ConfigError {
    pub fn mentions(&self, variable: &str) -> bool {
        self.problems.iter().any(|p| p.variable == variable)
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.problems.len();
        write!(
            f,
            "invalid configuration ({count} problem{}):",
            if count == 1 { "" } else { "s" }
        )?;
        for problem in &self.problems {
            write!(f, "\n  - {}: {}", problem.variable, problem.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

struct Problems(Vec<ConfigProblem>);

impl Problems {
    fn push(&mut self, variable: &'static str, message: impl Into<String>) {
        self.0.push(ConfigProblem {
            variable,
            message: message.into(),
        });
    }
}

impl Config {
    /// Loads configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_source(|name| std::env::var(name).ok())
    }

    /// Loads configuration through `lookup`. Blank values count as unset.
    pub fn from_source(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |name: &str| {
            lookup(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let mut problems = Problems(Vec::new());

        let environment = match get("TIKKIT_ENV").as_deref() {
            None => Environment::Development,
            Some(value) => match value.to_ascii_lowercase().as_str() {
                "development" => Environment::Development,
                "production" => Environment::Production,
                _ => {
                    problems.push(
                        "TIKKIT_ENV",
                        format!(
                            "{value:?} is not supported; use \"development\" or \"production\""
                        ),
                    );
                    Environment::Development
                }
            },
        };
        let production = environment.is_production();

        let (database_url, database_url_is_default) = match get("DATABASE_URL") {
            Some(value) => (parse_database_url(&value, production, &mut problems), false),
            None if production => {
                problems.push(
                    "DATABASE_URL",
                    "is required in production, e.g. postgres://tikkit:<password>@db:5432/tikkit",
                );
                (None, false)
            }
            None => (
                parse_database_url(DEVELOPMENT_DATABASE_URL, false, &mut problems),
                true,
            ),
        };

        let database_max_connections = parse_number(
            &get,
            "DATABASE_MAX_CONNECTIONS",
            10,
            MAX_CONNECTIONS_RANGE,
            &mut problems,
        ) as u32;
        let database_auto_migrate = parse_bool(&get, "DATABASE_AUTO_MIGRATE", true, &mut problems);
        let database_startup_timeout = Duration::from_secs(parse_number(
            &get,
            "DATABASE_STARTUP_TIMEOUT_SECS",
            30,
            STARTUP_TIMEOUT_RANGE,
            &mut problems,
        ));

        let api_addr = match get("API_ADDR") {
            None => DEFAULT_API_ADDR.parse().expect("valid default address"),
            Some(value) => value.parse().unwrap_or_else(|_| {
                problems.push(
                    "API_ADDR",
                    format!(
                        "{value:?} is not a socket address; use IP:PORT such as 127.0.0.1:3001 or 0.0.0.0:3001"
                    ),
                );
                DEFAULT_API_ADDR.parse().expect("valid default address")
            }),
        };

        let public_origin = match get("PUBLIC_ORIGIN") {
            Some(value) => parse_public_origin(&value, production, &mut problems),
            None if production => {
                problems.push(
                    "PUBLIC_ORIGIN",
                    "is required in production: the https origin users open, e.g. https://help.example.com",
                );
                String::new()
            }
            None => DEVELOPMENT_PUBLIC_ORIGIN.to_owned(),
        };

        let encryption_key = match get("TIKKIT_ENCRYPTION_KEY") {
            Some(value) => parse_encryption_key(&value, &mut problems),
            None if production => {
                problems.push(
                    "TIKKIT_ENCRYPTION_KEY",
                    "is required in production; generate one with `openssl rand -base64 32` and keep it outside Git",
                );
                None
            }
            None => None,
        };

        let shutdown_timeout = Duration::from_secs(parse_number(
            &get,
            "SHUTDOWN_TIMEOUT_SECS",
            20,
            SHUTDOWN_TIMEOUT_RANGE,
            &mut problems,
        ));
        let trust_proxy_headers = parse_bool(&get, "TRUST_PROXY_HEADERS", false, &mut problems);

        let log_filter = get("RUST_LOG").unwrap_or_else(|| DEFAULT_LOG_FILTER.to_owned());
        if let Err(error) = tracing_subscriber::EnvFilter::builder().parse(&log_filter) {
            problems.push(
                "RUST_LOG",
                format!("is not a valid log filter ({error}); for example use \"info\""),
            );
        }

        match (problems.0.is_empty(), database_url) {
            (true, Some(database_url)) => Ok(Self {
                environment,
                database_url,
                database_url_is_default,
                database_max_connections,
                database_auto_migrate,
                database_startup_timeout,
                api_addr,
                public_origin,
                encryption_key,
                shutdown_timeout,
                trust_proxy_headers,
                log_filter,
            }),
            _ => Err(ConfigError {
                problems: problems.0,
            }),
        }
    }

    /// A human-readable, redacted summary for `tikkit-api check-config`.
    pub fn summary(&self) -> String {
        let key = if self.encryption_key.is_some() {
            "set (redacted)"
        } else {
            "not set"
        };
        let default_note = if self.database_url_is_default {
            " (development default)"
        } else {
            ""
        };
        format!(
            "TIKKIT_ENV={}\nDATABASE_URL={}{}\nDATABASE_MAX_CONNECTIONS={}\nDATABASE_AUTO_MIGRATE={}\nDATABASE_STARTUP_TIMEOUT_SECS={}\nAPI_ADDR={}\nPUBLIC_ORIGIN={}\nTIKKIT_ENCRYPTION_KEY={}\nSHUTDOWN_TIMEOUT_SECS={}\nTRUST_PROXY_HEADERS={}\nRUST_LOG={}",
            self.environment.as_str(),
            self.database_url.redacted(),
            default_note,
            self.database_max_connections,
            self.database_auto_migrate,
            self.database_startup_timeout.as_secs(),
            self.api_addr,
            self.public_origin,
            key,
            self.shutdown_timeout.as_secs(),
            self.trust_proxy_headers,
            self.log_filter,
        )
    }
}

fn parse_database_url(
    value: &str,
    production: bool,
    problems: &mut Problems,
) -> Option<DatabaseUrl> {
    const VAR: &str = "DATABASE_URL";
    // Never echo the value: it may contain a password.
    let Ok(url) = Url::parse(value) else {
        problems.push(
            VAR,
            "is not a valid URL; expected postgres://USER:PASSWORD@HOST:PORT/DATABASE (special characters in the password must be percent-encoded)",
        );
        return None;
    };
    if !matches!(url.scheme(), "postgres" | "postgresql") {
        problems.push(
            VAR,
            format!(
                "uses the scheme {:?}; it must start with postgres:// or postgresql://",
                url.scheme()
            ),
        );
        return None;
    }
    if url.host_str().is_none_or(str::is_empty) && !url.query_pairs().any(|(k, _)| k == "host") {
        problems.push(
            VAR,
            "has no host; expected postgres://USER:PASSWORD@HOST:PORT/DATABASE",
        );
        return None;
    }
    let password = url
        .password()
        .map(|p| percent_decode(p).unwrap_or_else(|| p.to_owned()));
    if production && password.as_deref().is_some_and(looks_like_placeholder) {
        problems.push(
            VAR,
            "contains a placeholder password; set the real database password",
        );
    }
    if PgConnectOptions::from_str(value).is_err() {
        problems.push(VAR, "could not be parsed as PostgreSQL connection options");
        return None;
    }
    let mut redacted = url.clone();
    if url.password().is_some() {
        let _ = redacted.set_password(Some("***"));
    }
    let mut redacted = redacted.to_string();
    // Query parameters such as `password=` must not leak either.
    if url.query_pairs().any(|(k, _)| k == "password") {
        redacted = redacted.split('?').next().unwrap_or_default().to_owned() + "?***";
    }
    let password = password.or_else(|| {
        url.query_pairs()
            .find(|(k, _)| k == "password")
            .map(|(_, v)| v.into_owned())
    });
    Some(DatabaseUrl {
        raw: value.to_owned(),
        redacted,
        password,
    })
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn parse_public_origin(value: &str, production: bool, problems: &mut Problems) -> String {
    const VAR: &str = "PUBLIC_ORIGIN";
    let Ok(url) = Url::parse(value) else {
        problems.push(
            VAR,
            format!("{value:?} is not a URL; expected an origin such as https://help.example.com"),
        );
        return String::new();
    };
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        problems.push(
            VAR,
            format!("{value:?} must be an http:// or https:// origin with a host"),
        );
        return String::new();
    }
    if !url.username().is_empty() || url.password().is_some() {
        problems.push(VAR, "must not contain credentials");
        return String::new();
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        problems.push(
            VAR,
            format!(
                "{value:?} must be an origin only (scheme, host, optional port) without a path, query, or fragment"
            ),
        );
        return String::new();
    }
    if production && url.scheme() != "https" {
        problems.push(VAR, format!("{value:?} must use https:// in production"));
    }
    url.origin().ascii_serialization()
}

fn parse_encryption_key(value: &str, problems: &mut Problems) -> Option<EncryptionKey> {
    const VAR: &str = "TIKKIT_ENCRYPTION_KEY";
    // Never echo the value.
    let lower = value.to_ascii_lowercase();
    if looks_like_placeholder(value) || lower.contains("example") || lower.contains('<') {
        problems.push(
            VAR,
            "is a placeholder; generate a real key with `openssl rand -base64 32`",
        );
        return None;
    }
    let Ok(bytes) = BASE64.decode(value) else {
        problems.push(
            VAR,
            "is not valid base64; generate one with `openssl rand -base64 32`",
        );
        return None;
    };
    let Ok(bytes) = <[u8; ENCRYPTION_KEY_BYTES]>::try_from(bytes.as_slice()) else {
        problems.push(
            VAR,
            format!(
                "decodes to {} bytes but exactly {ENCRYPTION_KEY_BYTES} are required; generate one with `openssl rand -base64 32`",
                bytes.len()
            ),
        );
        return None;
    };
    let mut distinct = bytes.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    // 32 random bytes virtually always contain far more than 16 distinct values.
    if distinct.len() < 16 {
        problems.push(
            VAR,
            "is not random enough (too many repeated bytes); generate one with `openssl rand -base64 32`",
        );
        return None;
    }
    Some(EncryptionKey(bytes))
}

fn looks_like_placeholder(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "change_me",
        "changeme",
        "change-me",
        "replace_me",
        "replace-me",
        "replaceme",
        "placeholder",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn parse_bool(
    get: &impl Fn(&str) -> Option<String>,
    variable: &'static str,
    default: bool,
    problems: &mut Problems,
) -> bool {
    match get(variable) {
        None => default,
        Some(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => {
                problems.push(
                    variable,
                    format!("{value:?} is not a boolean; use true or false"),
                );
                default
            }
        },
    }
}

fn parse_number(
    get: &impl Fn(&str) -> Option<String>,
    variable: &'static str,
    default: u64,
    (min, max): (u64, u64),
    problems: &mut Problems,
) -> u64 {
    match get(variable) {
        None => default,
        Some(value) => match value.parse::<u64>() {
            Ok(number) if (min..=max).contains(&number) => number,
            _ => {
                problems.push(
                    variable,
                    format!("{value:?} must be a whole number from {min} to {max}"),
                );
                default
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const KEY: &str = "q8S2Pqf0x7e8hH3mD5bVb9w6vY1sT4nK2jL0aZcRxEo=";

    fn load(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_source(|name| map.get(name).cloned())
    }

    fn production(overrides: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let mut vars = vec![
            ("TIKKIT_ENV", "production"),
            (
                "DATABASE_URL",
                "postgres://tikkit:s3cr3t-db-pass@db:5432/tikkit",
            ),
            ("PUBLIC_ORIGIN", "https://help.example.org"),
            ("TIKKIT_ENCRYPTION_KEY", KEY),
            ("API_ADDR", "0.0.0.0:3001"),
        ];
        for (key, value) in overrides {
            vars.retain(|(k, _)| k != key);
            vars.push((key, value));
        }
        load(&vars)
    }

    #[test]
    fn development_defaults_need_no_variables() {
        let config = load(&[]).unwrap();
        assert_eq!(config.environment, Environment::Development);
        assert!(config.database_url_is_default);
        assert_eq!(config.database_url.expose(), DEVELOPMENT_DATABASE_URL);
        assert_eq!(config.api_addr.to_string(), "127.0.0.1:3001");
        assert_eq!(config.public_origin, "http://localhost:3000");
        assert!(config.encryption_key.is_none());
        assert_eq!(config.database_max_connections, 10);
        assert!(config.database_auto_migrate);
        assert_eq!(config.shutdown_timeout, Duration::from_secs(20));
        assert!(!config.trust_proxy_headers);
    }

    #[test]
    fn valid_production_config_loads_without_ai_or_smtp() {
        let config = production(&[]).unwrap();
        assert!(config.environment.is_production());
        assert!(!config.database_url_is_default);
        assert_eq!(config.public_origin, "https://help.example.org");
        assert_eq!(config.encryption_key.unwrap().expose().len(), 32);
    }

    #[test]
    fn production_requires_database_origin_and_key() {
        let error = load(&[("TIKKIT_ENV", "production")]).unwrap_err();
        assert!(error.mentions("DATABASE_URL"));
        assert!(error.mentions("PUBLIC_ORIGIN"));
        assert!(error.mentions("TIKKIT_ENCRYPTION_KEY"));
        assert_eq!(error.problems.len(), 3, "{error}");
    }

    #[test]
    fn all_problems_are_reported_together() {
        let error = load(&[
            ("TIKKIT_ENV", "staging"),
            ("DATABASE_URL", "mysql://root:hunter2@db/tikkit"),
            ("API_ADDR", "localhost"),
            ("PUBLIC_ORIGIN", "ftp://example.org"),
            ("DATABASE_MAX_CONNECTIONS", "0"),
            ("DATABASE_AUTO_MIGRATE", "maybe"),
            ("SHUTDOWN_TIMEOUT_SECS", "9999"),
            ("TRUST_PROXY_HEADERS", "2"),
        ])
        .unwrap_err();
        for var in [
            "TIKKIT_ENV",
            "DATABASE_URL",
            "API_ADDR",
            "PUBLIC_ORIGIN",
            "DATABASE_MAX_CONNECTIONS",
            "DATABASE_AUTO_MIGRATE",
            "SHUTDOWN_TIMEOUT_SECS",
            "TRUST_PROXY_HEADERS",
        ] {
            assert!(error.mentions(var), "missing {var} in {error}");
        }
        assert!(!error.to_string().contains("hunter2"));
    }

    #[test]
    fn database_url_must_be_postgres() {
        assert!(load(&[("DATABASE_URL", "postgresql://u:p@localhost/db")]).is_ok());
        assert!(
            load(&[(
                "DATABASE_URL",
                "postgres://u@localhost:6543/db?sslmode=require"
            )])
            .is_ok()
        );
        let error = load(&[("DATABASE_URL", "not a url p4ss")]).unwrap_err();
        assert!(error.mentions("DATABASE_URL"));
        assert!(!error.to_string().contains("p4ss"));
        let error = load(&[("DATABASE_URL", "https://u:p4ss@localhost/db")]).unwrap_err();
        assert!(error.to_string().contains("postgres://"));
        assert!(!error.to_string().contains("p4ss"));
    }

    #[test]
    fn placeholder_database_password_rejected_in_production() {
        let error = production(&[("DATABASE_URL", "postgres://tikkit:CHANGE_ME@db:5432/tikkit")])
            .unwrap_err();
        assert!(error.mentions("DATABASE_URL"));
    }

    #[test]
    fn public_origin_must_be_an_origin() {
        let config = load(&[("PUBLIC_ORIGIN", "http://localhost:3000/")]).unwrap();
        assert_eq!(config.public_origin, "http://localhost:3000");
        let config = load(&[("PUBLIC_ORIGIN", "HTTPS://Help.Example.org:443")]).unwrap();
        assert_eq!(config.public_origin, "https://help.example.org");
        for bad in [
            "https://example.org/portal",
            "https://example.org?x=1",
            "https://user:pw@example.org",
            "example.org",
            "ws://example.org",
        ] {
            assert!(
                load(&[("PUBLIC_ORIGIN", bad)])
                    .unwrap_err()
                    .mentions("PUBLIC_ORIGIN"),
                "{bad} should be rejected"
            );
        }
        let error = production(&[("PUBLIC_ORIGIN", "http://help.example.org")]).unwrap_err();
        assert!(error.to_string().contains("https"));
    }

    #[test]
    fn encryption_key_validation() {
        // Wrong length.
        let error = production(&[("TIKKIT_ENCRYPTION_KEY", "c2hvcnQ=")]).unwrap_err();
        assert!(error.to_string().contains("exactly 32"));
        // Not base64.
        let error = production(&[("TIKKIT_ENCRYPTION_KEY", "!!!not-base64!!!")]).unwrap_err();
        assert!(error.mentions("TIKKIT_ENCRYPTION_KEY"));
        // Placeholders and low-entropy keys.
        for bad in [
            "CHANGE_ME",
            "<generate with openssl rand -base64 32>",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        ] {
            let error = production(&[("TIKKIT_ENCRYPTION_KEY", bad)]).unwrap_err();
            assert!(error.mentions("TIKKIT_ENCRYPTION_KEY"), "{bad}");
        }
        // Optional but validated in development.
        assert!(load(&[("TIKKIT_ENCRYPTION_KEY", "c2hvcnQ=")]).is_err());
        assert!(load(&[("TIKKIT_ENCRYPTION_KEY", KEY)]).is_ok());
    }

    #[test]
    fn numeric_bounds_and_booleans() {
        let config = load(&[
            ("DATABASE_MAX_CONNECTIONS", "100"),
            ("SHUTDOWN_TIMEOUT_SECS", "1"),
            ("DATABASE_AUTO_MIGRATE", "false"),
            ("TRUST_PROXY_HEADERS", "TRUE"),
        ])
        .unwrap();
        assert_eq!(config.database_max_connections, 100);
        assert_eq!(config.shutdown_timeout, Duration::from_secs(1));
        assert!(!config.database_auto_migrate);
        assert!(config.trust_proxy_headers);
        assert!(load(&[("DATABASE_MAX_CONNECTIONS", "101")]).is_err());
        assert!(load(&[("SHUTDOWN_TIMEOUT_SECS", "0")]).is_err());
        assert!(load(&[("SHUTDOWN_TIMEOUT_SECS", "-5")]).is_err());
    }

    #[test]
    fn blank_values_are_unset() {
        let config = load(&[("API_ADDR", "  "), ("TIKKIT_ENCRYPTION_KEY", "")]).unwrap();
        assert_eq!(config.api_addr.to_string(), DEFAULT_API_ADDR);
        assert!(config.encryption_key.is_none());
    }

    #[test]
    fn invalid_log_filter_rejected() {
        assert!(load(&[("RUST_LOG", "info,tikkit_api=debug")]).is_ok());
        assert!(
            load(&[("RUST_LOG", "=[")])
                .unwrap_err()
                .mentions("RUST_LOG")
        );
    }

    #[test]
    fn debug_and_summary_redact_secrets() {
        let config = production(&[(
            "DATABASE_URL",
            "postgres://tikkit:s3cr3t%40pass@db:5432/tikkit",
        )])
        .unwrap();
        let debug = format!("{config:?}");
        let summary = config.summary();
        for text in [&debug, &summary] {
            assert!(!text.contains("s3cr3t"), "{text}");
            assert!(!text.contains(KEY), "{text}");
            assert!(text.contains("***") || text.contains("redacted"), "{text}");
        }
        assert!(debug.contains("db:5432/tikkit"));
        assert_eq!(
            config
                .database_url
                .scrub("password authentication failed: s3cr3t@pass"),
            "password authentication failed: ***"
        );
    }

    fn parse_env_file(text: &str) -> HashMap<String, String> {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
            .collect()
    }

    #[test]
    fn production_env_example_is_valid_once_placeholders_are_replaced() {
        let mut vars = parse_env_file(include_str!("../../../deploy/compose.env.example"));
        let error = Config::from_source(|name| vars.get(name).cloned()).unwrap_err();
        // Only the placeholders are rejected, and without echoing them.
        assert!(error.mentions("DATABASE_URL"), "{error}");
        assert!(error.mentions("TIKKIT_ENCRYPTION_KEY"), "{error}");
        assert_eq!(error.problems.len(), 2, "{error}");

        vars.insert(
            "DATABASE_URL".into(),
            "postgres://tikkit:Zt5qkV2wP9xL@db:5432/tikkit".into(),
        );
        vars.insert("TIKKIT_ENCRYPTION_KEY".into(), KEY.into());
        let config = Config::from_source(|name| vars.get(name).cloned()).unwrap();
        assert!(config.environment.is_production());
        assert_eq!(config.api_addr.to_string(), "0.0.0.0:3001");
    }

    #[test]
    fn development_env_example_is_valid() {
        let vars = parse_env_file(include_str!("../../../.env.example"));
        let config = Config::from_source(|name| vars.get(name).cloned()).unwrap();
        assert_eq!(config.environment, Environment::Development);
    }

    #[test]
    fn password_in_query_is_redacted() {
        let config = load(&[(
            "DATABASE_URL",
            "postgres://db.internal/tikkit?user=tikkit&password=qu3ry-secret",
        )])
        .unwrap();
        assert!(!format!("{config:?}").contains("qu3ry-secret"));
        assert_eq!(config.database_url.scrub("x qu3ry-secret y"), "x *** y");
    }
}

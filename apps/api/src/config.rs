use std::{fmt, net::SocketAddr, str::FromStr, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use sqlx::postgres::PgConnectOptions;
use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Environment {
    Development,
    Production,
}

/// Validated runtime configuration. Secrets are redacted from `Debug`.
pub struct Config {
    pub database_url: String,
    pub api_addr: SocketAddr,
    pub public_origin: Url,
    pub session_secret: Vec<u8>,
    pub encryption_key: [u8; 32],
    pub shutdown_timeout: Duration,
    pub environment: Environment,
    /// Absolute session lifetime.
    pub session_ttl: Duration,
    /// Session age after which the token is rotated.
    pub session_rotate: Duration,
    /// Take the client IP from `X-Forwarded-For` (only behind a trusted reverse proxy).
    pub trust_proxy_headers: bool,
}

/// Every configuration problem found, one actionable line each. Never contains values.
#[derive(Debug)]
pub struct ConfigError(pub Vec<String>);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid configuration (see docs/configuration.md):")?;
        self.0.iter().try_for_each(|e| write!(f, "\n  - {e}"))
    }
}

impl std::error::Error for ConfigError {}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("database_url", &"<redacted>")
            .field("api_addr", &self.api_addr)
            .field("public_origin", &self.public_origin.as_str())
            .field("session_secret", &"<redacted>")
            .field("encryption_key", &"<redacted>")
            .field("shutdown_timeout", &self.shutdown_timeout)
            .field("environment", &self.environment)
            .field("session_ttl", &self.session_ttl)
            .field("session_rotate", &self.session_rotate)
            .field("trust_proxy_headers", &self.trust_proxy_headers)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_vars(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty()))
    }

    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut errors = Vec::new();
        let mut fail = |msg: &str| errors.push(msg.to_owned());

        let environment = match var("ENVIRONMENT").as_deref() {
            None | Some("development") => Environment::Development,
            Some("production") => Environment::Production,
            Some(_) => {
                fail("ENVIRONMENT must be `development` or `production`");
                Environment::Development
            }
        };
        let database_url = var("DATABASE_URL").unwrap_or_default();
        if database_url.is_empty() {
            fail("DATABASE_URL is required, e.g. postgres://tikkit:PASSWORD@db-host:5432/tikkit");
        } else if !database_url.starts_with("postgres")
            || PgConnectOptions::from_str(&database_url).is_err()
        {
            fail("DATABASE_URL must be a postgres:// or postgresql:// connection URL");
        }
        let api_addr = var("API_ADDR")
            .unwrap_or_else(|| "127.0.0.1:3001".into())
            .parse()
            .unwrap_or_else(|_| {
                fail(
                    "API_ADDR must be an IP socket address such as 127.0.0.1:3001 or 0.0.0.0:3001",
                );
                ([127, 0, 0, 1], 3001).into()
            });
        let public_origin = match var("PUBLIC_ORIGIN").map(|v| Url::parse(&v)) {
            None => {
                fail(
                    "PUBLIC_ORIGIN is required: the URL users open, e.g. https://help.example.com",
                );
                None
            }
            Some(Ok(url))
                if matches!(url.scheme(), "http" | "https")
                    && url.has_host()
                    && url.path() == "/"
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && url.username().is_empty()
                    && url.password().is_none() =>
            {
                if environment == Environment::Production && url.scheme() != "https" {
                    fail("PUBLIC_ORIGIN must use https:// when ENVIRONMENT=production");
                }
                Some(url)
            }
            Some(_) => {
                fail(
                    "PUBLIC_ORIGIN must be an http(s) origin without path, query, or credentials, e.g. https://help.example.com",
                );
                None
            }
        };
        let session_secret = var("SESSION_SECRET").and_then(|v| decode_secret(&v));
        if session_secret.as_ref().is_none_or(|s| s.len() < 32) {
            fail(
                "SESSION_SECRET must be at least 32 random bytes, base64 or hex encoded; generate one with `openssl rand -base64 32`",
            );
        }
        let encryption_key = var("ENCRYPTION_KEY")
            .and_then(|v| decode_secret(&v))
            .and_then(|k| <[u8; 32]>::try_from(k).ok());
        if encryption_key.is_none() {
            fail(
                "ENCRYPTION_KEY must be exactly 32 random bytes, base64 or hex encoded; generate one with `openssl rand -base64 32` and back it up separately (losing it makes stored credentials unreadable)",
            );
        }
        let shutdown_timeout = match var("SHUTDOWN_TIMEOUT_SECS").map(|v| v.parse::<u64>()) {
            None => Duration::from_secs(20),
            Some(Ok(secs)) if (1..=600).contains(&secs) => Duration::from_secs(secs),
            Some(_) => {
                fail("SHUTDOWN_TIMEOUT_SECS must be a whole number of seconds between 1 and 600");
                Duration::from_secs(20)
            }
        };
        let mut hours = |name: &str, default: u64, max: u64| match var(name).map(|v| v.parse()) {
            None => Duration::from_secs(default * 3600),
            Some(Ok(h)) if (1..=max).contains(&h) => Duration::from_secs(h * 3600),
            Some(_) => {
                fail(&format!(
                    "{name} must be a whole number of hours between 1 and {max}"
                ));
                Duration::from_secs(default * 3600)
            }
        };
        let session_ttl = hours("SESSION_TTL_HOURS", 24 * 14, 24 * 365);
        let session_rotate = hours("SESSION_ROTATE_HOURS", 24, 24 * 365);
        let trust_proxy_headers = match var("TRUST_PROXY_HEADERS").as_deref() {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => {
                fail("TRUST_PROXY_HEADERS must be `true` or `false`");
                false
            }
        };
        match (public_origin, session_secret, encryption_key) {
            (Some(public_origin), Some(session_secret), Some(encryption_key))
                if errors.is_empty() =>
            {
                Ok(Self {
                    database_url,
                    api_addr,
                    public_origin,
                    session_secret,
                    encryption_key,
                    shutdown_timeout,
                    environment,
                    session_ttl,
                    session_rotate,
                    trust_proxy_headers,
                })
            }
            _ => Err(ConfigError(errors)),
        }
    }
}

/// Hex if the value is entirely hex digits of even length, otherwise standard base64.
fn decode_secret(value: &str) -> Option<Vec<u8>> {
    let value = value.trim();
    if value.len().is_multiple_of(2) && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return (0..value.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&value[i..i + 2], 16).ok())
            .collect();
    }
    STANDARD.decode(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_B64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
    const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    fn config(overrides: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let mut vars = vec![
            ("DATABASE_URL", "postgres://tikkit:hunter2@db:5432/tikkit"),
            ("PUBLIC_ORIGIN", "https://help.example.com"),
            ("SESSION_SECRET", KEY_B64),
            ("ENCRYPTION_KEY", KEY_HEX),
        ];
        vars.extend_from_slice(overrides);
        Config::from_vars(|name| {
            vars.iter()
                .rev()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
                .filter(|v| !v.is_empty())
        })
    }

    #[test]
    fn valid_configuration_uses_defaults() {
        let c = config(&[]).unwrap();
        assert_eq!(c.api_addr, "127.0.0.1:3001".parse().unwrap());
        assert_eq!(c.shutdown_timeout, Duration::from_secs(20));
        assert_eq!(c.environment, Environment::Development);
        assert_eq!(c.encryption_key[31], 31);
        assert_eq!(c.session_secret.len(), 32);
        assert_eq!(c.session_ttl, Duration::from_secs(14 * 24 * 3600));
        assert_eq!(c.session_rotate, Duration::from_secs(24 * 3600));
        assert!(!c.trust_proxy_headers);
        let debug = format!("{c:?}");
        assert!(!debug.contains("hunter2") && !debug.contains(KEY_B64));
    }

    #[test]
    fn all_errors_are_reported_without_values() {
        let err = config(&[
            ("DATABASE_URL", "mysql://root:hunter2@db/x"),
            ("API_ADDR", "localhost"),
            ("PUBLIC_ORIGIN", "https://help.example.com/portal"),
            ("SESSION_SECRET", "c2hvcnQ="),
            ("ENCRYPTION_KEY", "CHANGE_ME_openssl_rand_base64_32"),
            ("SHUTDOWN_TIMEOUT_SECS", "0"),
            ("ENVIRONMENT", "staging"),
            ("SESSION_TTL_HOURS", "0"),
            ("SESSION_ROTATE_HOURS", "soon"),
            ("TRUST_PROXY_HEADERS", "yes"),
        ])
        .unwrap_err();
        assert_eq!(err.0.len(), 10, "{err}");
        let text = err.to_string();
        assert!(!text.contains("hunter2") && !text.contains("c2hvcnQ="));
    }

    #[test]
    fn missing_required_values_are_reported() {
        let err = config(&[
            ("DATABASE_URL", ""),
            ("PUBLIC_ORIGIN", ""),
            ("SESSION_SECRET", ""),
            ("ENCRYPTION_KEY", ""),
        ])
        .unwrap_err();
        assert_eq!(err.0.len(), 4, "{err}");
    }

    #[test]
    fn production_requires_https_origin() {
        let err = config(&[
            ("ENVIRONMENT", "production"),
            ("PUBLIC_ORIGIN", "http://help.example.com"),
        ])
        .unwrap_err();
        assert!(err.0[0].contains("https"));
        assert!(config(&[("ENVIRONMENT", "production")]).is_ok());
    }
}

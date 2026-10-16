//! The session cookie.
//!
//! - Name `tikkit_session` in development; `__Host-tikkit_session` with
//!   `Secure` in production (the `__Host-` prefix pins it to this exact
//!   origin, `Path=/`, no `Domain`).
//! - Always `HttpOnly; SameSite=Lax; Path=/` and a `Max-Age` equal to the
//!   remaining absolute session lifetime.
//!
//! Handlers and extractors never write `Set-Cookie` themselves. They record
//! the desired change in the request's [`SessionCookies`] slot, and
//! [`middleware`] renders it onto the response (together with
//! `Cache-Control: no-store`). This lets the [`Actor`](super::Actor)
//! extractor rotate or clear the cookie whatever the handler returns.

use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
};

use axum::{
    extract::{FromRequestParts, Request, State},
    http::{HeaderMap, HeaderValue, header, request::Parts},
    middleware::Next,
    response::Response,
};

use crate::{AppState, config::Config};

pub const DEVELOPMENT_COOKIE_NAME: &str = "tikkit_session";
pub const PRODUCTION_COOKIE_NAME: &str = "__Host-tikkit_session";

/// Cookie name for this deployment.
pub fn cookie_name(config: &Config) -> &'static str {
    if config.environment.is_production() {
        PRODUCTION_COOKIE_NAME
    } else {
        DEVELOPMENT_COOKIE_NAME
    }
}

/// The raw session token presented by the client, if any. Only the cookie
/// with this deployment's name is considered.
pub fn read_token(headers: &HeaderMap, config: &Config) -> Option<String> {
    let name = cookie_name(config);
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.trim().trim_matches('"').to_owned())
        .filter(|value| !value.is_empty())
}

/// A pending change to the session cookie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CookieChange {
    Set { token: String, max_age_secs: i64 },
    Clear,
}

/// `Set-Cookie` header value for `change`.
pub fn render(change: &CookieChange, config: &Config) -> HeaderValue {
    let name = cookie_name(config);
    let secure = if config.environment.is_production() {
        "; Secure"
    } else {
        ""
    };
    let value = match change {
        CookieChange::Set {
            token,
            max_age_secs,
        } => format!(
            "{name}={token}; Path=/; Max-Age={}; HttpOnly; SameSite=Lax{secure}",
            (*max_age_secs).max(0)
        ),
        CookieChange::Clear => {
            format!("{name}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax{secure}")
        }
    };
    HeaderValue::from_str(&value).expect("cookie values are ASCII")
}

/// Request-scoped slot for the response's session cookie change. Extract it
/// in handlers (`cookies: SessionCookies`) and call [`Self::set`] or
/// [`Self::clear`]. The last change wins.
#[derive(Clone, Debug, Default)]
pub struct SessionCookies(Arc<Mutex<Option<CookieChange>>>);

impl SessionCookies {
    pub fn set(&self, token: String, max_age_secs: i64) {
        *self.lock() = Some(CookieChange::Set {
            token,
            max_age_secs,
        });
    }

    pub fn clear(&self) {
        *self.lock() = Some(CookieChange::Clear);
    }

    pub fn pending(&self) -> Option<CookieChange> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<CookieChange>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The slot of this request. Without [`middleware`] installed, a detached
    /// slot is returned (changes are then silently dropped).
    pub fn of(parts: &Parts) -> Self {
        parts.extensions.get::<Self>().cloned().unwrap_or_default()
    }
}

impl<S: Send + Sync> FromRequestParts<S> for SessionCookies {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::of(parts))
    }
}

/// Installs the [`SessionCookies`] slot and renders its change onto the
/// response. Responses that change the cookie are never cached.
pub async fn middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let slot = SessionCookies::default();
    request.extensions_mut().insert(slot.clone());
    let mut response = next.run(request).await;
    if let Some(change) = slot.pending() {
        let headers = response.headers_mut();
        headers.append(header::SET_COOKIE, render(&change, &state.config));
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(env: &str) -> Config {
        let env = env.to_owned();
        Config::from_source(move |name| match name {
            "DATABASE_URL" => Some("postgres://u:p@db:5432/tikkit".into()),
            "TIKKIT_ENV" => Some(env.clone()),
            "PUBLIC_ORIGIN" => Some("https://help.example.test".into()),
            "TIKKIT_ENCRYPTION_KEY" => Some("MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=".into()),
            _ => None,
        })
        .unwrap()
    }

    #[test]
    fn renders_attributes_per_environment() {
        let set = CookieChange::Set {
            token: "abc".into(),
            max_age_secs: 60,
        };
        assert_eq!(
            render(&set, &config("development")),
            "tikkit_session=abc; Path=/; Max-Age=60; HttpOnly; SameSite=Lax"
        );
        assert_eq!(
            render(&set, &config("production")),
            "__Host-tikkit_session=abc; Path=/; Max-Age=60; HttpOnly; SameSite=Lax; Secure"
        );
        assert_eq!(
            render(&CookieChange::Clear, &config("production")),
            "__Host-tikkit_session=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax; Secure"
        );
    }

    #[test]
    fn reads_only_the_configured_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("tikkit_lang=de; tikkit_session=tok1; other=x"),
        );
        assert_eq!(
            read_token(&headers, &config("development")).as_deref(),
            Some("tok1")
        );
        assert_eq!(read_token(&headers, &config("production")), None);
    }
}

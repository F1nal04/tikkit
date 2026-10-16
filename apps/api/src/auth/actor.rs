//! The authenticated-actor extractor.
//!
//! Add `actor: Actor` to a handler to require a valid session (401
//! `unauthenticated` otherwise), or `actor: Option<Actor>` for endpoints that
//! work with and without one. On every request the session is validated and
//! the account is reloaded from the database, so role changes apply on the
//! next request and deactivated accounts lose access immediately (their
//! session is revoked). Expired or invalid cookies are cleared; due tokens
//! are rotated transparently (see [`super::sessions`]).
//!
//! Authorization decisions use `actor.account_id` (never username or email).

use axum::{
    extract::{FromRequestParts, OptionalFromRequestParts},
    http::request::Parts,
};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{cookie, sessions};
use crate::{AppState, domain::Role, dto::Language, error::ApiError};

/// The signed-in account making the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    pub account_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub language: Language,
    pub session_id: Uuid,
}

/// Session details resolved together with the [`Actor`], available as a
/// request extension (`Extension<SessionDetails>`) after the extractor ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionDetails {
    pub session_id: Uuid,
    pub expires_at: OffsetDateTime,
}

impl Actor {
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
}

/// Resolves the actor, recording cookie changes (rotation/clear) in the
/// request's [`cookie::SessionCookies`] slot.
async fn resolve(parts: &mut Parts, state: &AppState) -> Result<Option<Actor>, ApiError> {
    if let Some(actor) = parts.extensions.get::<Actor>() {
        return Ok(Some(actor.clone()));
    }
    let Some(token) = cookie::read_token(&parts.headers, &state.config) else {
        return Ok(None);
    };
    let cookies = cookie::SessionCookies::of(parts);
    let Some(session) =
        sessions::authenticate(&state.db, &token, &state.auth.session_policy).await?
    else {
        cookies.clear();
        return Ok(None);
    };
    if let Some((token, max_age)) = session.rotated {
        cookies.set(token, max_age);
    }
    let actor = Actor {
        account_id: session.account_id,
        username: session.username,
        display_name: session.display_name,
        role: session.role,
        language: session.language,
        session_id: session.session_id,
    };
    parts.extensions.insert(actor.clone());
    parts.extensions.insert(SessionDetails {
        session_id: session.session_id,
        expires_at: session.expires_at,
    });
    Ok(Some(actor))
}

impl FromRequestParts<AppState> for Actor {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        resolve(parts, state)
            .await?
            .ok_or_else(ApiError::unauthenticated)
    }
}

impl OptionalFromRequestParts<AppState> for Actor {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, Self::Rejection> {
        resolve(parts, state).await
    }
}

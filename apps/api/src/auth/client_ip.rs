//! Client address used for rate limiting and audit metadata.
//!
//! By default this is the TCP peer address (`ConnectInfo`, set by
//! `server::serve`). With `TRUST_PROXY_HEADERS=true` it is the right-most
//! valid address in `X-Forwarded-For`, i.e. the address the (single, trusted)
//! reverse proxy saw. Routers driven without a socket (tests) have no
//! address; such requests share one rate-limit key.

use std::{
    convert::Infallible,
    net::{IpAddr, SocketAddr},
};

use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::request::Parts,
};

use crate::AppState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientIp(pub Option<IpAddr>);

impl ClientIp {
    /// Stable key for rate limiting.
    pub fn key(&self) -> String {
        match self.0 {
            Some(ip) => ip.to_string(),
            None => "unknown".to_owned(),
        }
    }

    pub fn from_parts(parts: &Parts, trust_proxy_headers: bool) -> Self {
        if trust_proxy_headers {
            let forwarded = parts
                .headers
                .get_all("x-forwarded-for")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|v| v.split(','))
                .filter_map(|v| v.trim().parse::<IpAddr>().ok())
                .next_back();
            if let Some(ip) = forwarded {
                return Self(Some(ip));
            }
        }
        Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(addr)| addr.ip()),
        )
    }
}

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self::from_parts(parts, state.config.trust_proxy_headers))
    }
}

//! Web/API boundary controls: which peers may supply forwarding headers, the
//! same-origin policy for state-changing requests, response security headers,
//! and per-account throttles on costly routes. See docs/security.md.

use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use axum::{
    Extension,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{
    api::ApiError,
    auth::{Auth, CurrentUser},
};

/// Peers allowed to supply `X-Forwarded-For` (`TRUSTED_PROXIES`): the public
/// reverse proxy and the SSR server. Empty means no forwarding header is honoured.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<(IpAddr, u8)>);

impl fmt::Debug for TrustedProxies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nets: Vec<String> = self.0.iter().map(|(ip, p)| format!("{ip}/{p}")).collect();
        f.debug_tuple("TrustedProxies").field(&nets).finish()
    }
}

impl FromStr for TrustedProxies {
    type Err = ();

    /// Comma-separated IP addresses or CIDR networks, e.g. `172.30.0.0/24, 10.0.0.5`.
    fn from_str(value: &str) -> Result<Self, ()> {
        let mut nets = Vec::new();
        for item in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (ip, prefix) = match item.split_once('/') {
                Some((ip, prefix)) => (ip, Some(prefix)),
                None => (item, None),
            };
            let ip: IpAddr = ip.parse().map_err(|_| ())?;
            let max = if ip.is_ipv4() { 32 } else { 128 };
            let prefix = match prefix {
                Some(p) => p.parse::<u8>().map_err(|_| ())?,
                None => max,
            };
            if prefix > max {
                return Err(());
            }
            // A catch-all would trust every client's own header.
            if prefix == 0 {
                return Err(());
            }
            nets.push((canonical(ip), prefix));
        }
        Ok(Self(nets))
    }
}

impl TrustedProxies {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        self.0.iter().any(|&(net, prefix)| match (net, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX << (32 - u32::from(prefix));
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX << (128 - u32::from(prefix));
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        })
    }

    /// The client address for rate limits and audit records. The socket peer,
    /// unless it is a trusted proxy: then `X-Forwarded-For` is read from the
    /// right, skipping trusted hops, and the first untrusted address is the
    /// client. Anything the client wrote to the left of that is ignored, and a
    /// malformed entry stops the walk at the last trusted hop.
    pub fn client_ip(&self, peer: Option<IpAddr>, forwarded_for: &[&str]) -> Option<IpAddr> {
        let peer = canonical(peer?);
        if !self.contains(peer) {
            return Some(peer);
        }
        let mut client = peer;
        for hop in forwarded_for.iter().rev().flat_map(|h| h.rsplit(',')) {
            match hop.trim().parse::<IpAddr>() {
                Ok(ip) => {
                    client = canonical(ip);
                    if !self.contains(client) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        Some(client)
    }
}

/// IPv4-mapped IPv6 (`::ffff:a.b.c.d`) is the IPv4 address it carries.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// The client IP of a request (see [`TrustedProxies::client_ip`]).
pub fn request_client_ip(
    headers: &HeaderMap,
    connect: Option<&ConnectInfo<SocketAddr>>,
    trusted: &TrustedProxies,
) -> Option<IpAddr> {
    let forwarded: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    trusted.client_ip(connect.map(|c| c.0.ip()), &forwarded)
}

/// Whether a state-changing request comes from the portal's own origin, as far
/// as the browser tells us. Requests without `Origin` and `Sec-Fetch-Site`
/// (SSR, scripts, tests) pass; the `X-Requested-With` header still applies.
pub fn same_origin(headers: &HeaderMap, public_origin: &str) -> bool {
    let site_ok = match headers.get("sec-fetch-site").map(HeaderValue::as_bytes) {
        None | Some(b"same-origin") => true,
        Some(_) => false,
    };
    let origin_ok = match headers.get(header::ORIGIN) {
        None => true,
        Some(origin) => origin
            .to_str()
            .is_ok_and(|o| o.eq_ignore_ascii_case(public_origin)),
    };
    site_ok && origin_ok
}

/// Security headers on every API response. Handlers that serve their own
/// policy (screenshot files) keep theirs; nothing here is ever removed.
pub async fn headers(State(hsts): State<bool>, request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    let mut set = |name: HeaderName, value: &'static str| {
        headers
            .entry(name)
            .or_insert(HeaderValue::from_static(value));
    };
    set(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(header::X_FRAME_OPTIONS, "DENY");
    set(
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
    );
    set(header::REFERRER_POLICY, "no-referrer");
    set(
        HeaderName::from_static("cross-origin-resource-policy"),
        "same-origin",
    );
    set(
        HeaderName::from_static("cross-origin-opener-policy"),
        "same-origin",
    );
    if hsts {
        set(header::STRICT_TRANSPORT_SECURITY, "max-age=31536000");
    }
    response
}

/// Upper bound on handling one API request, body read included. Longer than
/// every outbound call a handler makes (AI probe 15 s, SMTP 15 s, upload body
/// 30 s), so it only cuts off requests that are stuck or deliberately slow.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Ends a request still running after [`REQUEST_TIMEOUT`] with `503
/// request_timeout`. Dropping the handler rolls back its open transaction,
/// exactly as when the client disconnects.
pub async fn deadline(request: Request, next: Next) -> Response {
    bounded(REQUEST_TIMEOUT, next.run(request)).await
}

async fn bounded(limit: Duration, response: impl Future<Output = Response>) -> Response {
    match tokio::time::timeout(limit, response).await {
        Ok(response) => response,
        Err(_) => ApiError::RequestTimeout.into_response(),
    }
}

/// A per-account budget for a costly route, enforced before the body is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Throttle {
    /// Ticket submission (direct or from a draft).
    Submission,
    /// Draft creation and saves (autosave included).
    Draft,
    /// Public replies and internal notes.
    Message,
    /// Screenshot uploads.
    Upload,
    /// Queue lists and search.
    Search,
    /// CSV exports.
    Export,
    /// Admin connection tests that reach an outside AI provider or SMTP relay.
    ConnectionTest,
}

impl Throttle {
    /// `(scope, requests per minute per account)`.
    pub const fn budget(self) -> (&'static str, u32) {
        match self {
            Self::Submission => ("submission", 10),
            Self::Draft => ("draft", 60),
            Self::Message => ("message", 30),
            Self::Upload => ("upload", 30),
            Self::Search => ("search", 120),
            Self::Export => ("export", 6),
            Self::ConnectionTest => ("connection_test", 5),
        }
    }
}

/// Route layer for [`Throttle`]. Signed-out requests pass through to the
/// handler's own `401`; they never create limiter keys.
pub async fn throttle(
    State(throttle): State<Throttle>,
    Extension(auth): Extension<Arc<Auth>>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(user) = request.extensions().get::<CurrentUser>()
        && let Err(error) = auth.limit_account(throttle, user.account_id())
    {
        return error.into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn trusted_proxies_parse_addresses_and_networks() {
        let t: TrustedProxies = "172.30.0.0/24, 10.0.0.5 ,fd00::/8".parse().unwrap();
        assert!(t.contains(ip("172.30.0.9")) && !t.contains(ip("172.30.1.9")));
        assert!(t.contains(ip("10.0.0.5")) && !t.contains(ip("10.0.0.6")));
        assert!(t.contains(ip("fd12::1")) && !t.contains(ip("fe80::1")));
        assert!(t.contains(ip("::ffff:10.0.0.5")), "mapped IPv4");
        assert!("".parse::<TrustedProxies>().unwrap().is_empty());
        for bad in [
            "0.0.0.0/0",
            "::/0",
            "10.0.0.0/33",
            "proxy",
            "10.0.0.1/x",
            "*",
        ] {
            assert!(bad.parse::<TrustedProxies>().is_err(), "{bad}");
        }
    }

    #[test]
    fn forwarded_for_counts_only_behind_a_trusted_peer() {
        let none = TrustedProxies::default();
        let t: TrustedProxies = "172.30.0.0/24".parse().unwrap();
        let caddy = Some(ip("172.30.0.2"));
        let outsider = Some(ip("203.0.113.9"));
        // Untrusted peers: the header is the client's own claim and is ignored.
        assert_eq!(none.client_ip(caddy, &["198.51.100.1"]), caddy);
        assert_eq!(t.client_ip(outsider, &["198.51.100.1"]), outsider);
        // Trusted proxy: the rightmost untrusted hop is the client.
        assert_eq!(
            t.client_ip(caddy, &["198.51.100.1"]),
            Some(ip("198.51.100.1"))
        );
        // A spoofed entry the proxy appended to is skipped.
        let spoofed = ["10.9.9.9, 198.51.100.1"];
        assert_eq!(t.client_ip(caddy, &spoofed), Some(ip("198.51.100.1")));
        // Proxy -> SSR server -> API: both hops trusted.
        let chain = ["198.51.100.1, 172.30.0.2"];
        assert_eq!(
            t.client_ip(Some(ip("172.30.0.3")), &chain),
            Some(ip("198.51.100.1"))
        );
        // Repeated headers read as one list.
        let split = ["10.9.9.9", "198.51.100.1"];
        assert_eq!(t.client_ip(caddy, &split), Some(ip("198.51.100.1")));
        // Garbage never becomes the client address.
        assert_eq!(t.client_ip(caddy, &["<script>"]), caddy);
        assert_eq!(
            t.client_ip(caddy, &["unknown, 172.30.0.7"]),
            Some(ip("172.30.0.7"))
        );
        assert_eq!(t.client_ip(caddy, &[]), caddy);
        assert_eq!(t.client_ip(None, &["198.51.100.1"]), None);
    }

    #[tokio::test]
    async fn slow_requests_end_with_request_timeout() {
        let slow = async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            "late".into_response()
        };
        let response = bounded(Duration::from_millis(20), slow).await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        let fast = bounded(Duration::from_secs(5), async { "ok".into_response() }).await;
        assert_eq!(fast.status(), axum::http::StatusCode::OK);
    }

    #[test]
    fn same_origin_requires_matching_browser_metadata() {
        let origin = "https://help.example.com";
        let check = |pairs: &[(&'static str, &'static str)]| {
            let mut h = HeaderMap::new();
            for (k, v) in pairs {
                h.insert(*k, HeaderValue::from_static(v));
            }
            same_origin(&h, origin)
        };
        assert!(check(&[]));
        assert!(check(&[("origin", "https://help.example.com")]));
        assert!(check(&[("sec-fetch-site", "same-origin")]));
        assert!(!check(&[("origin", "https://evil.example")]));
        assert!(!check(&[("origin", "null")]));
        assert!(!check(&[("origin", "http://help.example.com")]));
        assert!(!check(&[(
            "origin",
            "https://help.example.com.evil.example"
        )]));
        assert!(!check(&[("sec-fetch-site", "cross-site")]));
        assert!(!check(&[("sec-fetch-site", "same-site")]));
        assert!(!check(&[
            ("origin", "https://help.example.com"),
            ("sec-fetch-site", "cross-site")
        ]));
    }
}

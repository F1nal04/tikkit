//! Outbound HTTP for provider calls: destination policy, no redirects, bounded
//! reads. See docs/ai-providers.md ("Outbound policy").

use std::{
    error::Error as StdError,
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{Arc, OnceLock},
    time::Duration,
};

use reqwest::{
    Client, Response,
    dns::{Addrs, Name, Resolve, Resolving},
    redirect,
};
use url::{Host, Url};

use super::{AiError, AiErrorKind, Endpoint};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A validated request destination.
pub(crate) struct Target {
    pub url: Url,
    /// True only for an override that explicitly allows private networks.
    pub private: bool,
}

/// Build the request URL for `path` and enforce the destination policy that can
/// be checked without DNS. Hostnames are checked again at resolution time.
pub(crate) fn target(
    endpoint: &Endpoint,
    official: &'static str,
    path: &str,
) -> Result<Target, AiError> {
    let blocked = || AiError::new(AiErrorKind::DestinationBlocked);
    let (base, private) = match endpoint {
        Endpoint::Official => (Url::parse(official).map_err(|_| blocked())?, false),
        Endpoint::Override {
            base_url,
            allow_private_network,
        } => (base_url.clone(), *allow_private_network),
    };
    if !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return Err(blocked());
    }
    match base.scheme() {
        "https" => {}
        "http" if private => {}
        _ => return Err(blocked()),
    }
    if !private {
        let public = match base.host() {
            Some(Host::Ipv4(ip)) => is_public(IpAddr::V4(ip)),
            Some(Host::Ipv6(ip)) => is_public(IpAddr::V6(ip)),
            Some(Host::Domain(name)) => !name.is_empty(),
            None => false,
        };
        if !public {
            return Err(blocked());
        }
    }
    let joined = format!("{}{path}", base.as_str().trim_end_matches('/'));
    let url = Url::parse(&joined).map_err(|_| blocked())?;
    Ok(Target { url, private })
}

/// True for addresses on the public internet. Everything loopback, private,
/// link-local, CGNAT, multicast, reserved, or documentation-only is refused,
/// including IPv4 addresses embedded in IPv6.
pub(crate) fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => is_public_v6(ip),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || (a == 100 && (64..128).contains(&b)) // 100.64.0.0/10 CGNAT
        || (a == 192 && b == 0 && c == 0) // 192.0.0.0/24
        || (a == 198 && (18..20).contains(&b)) // 198.18.0.0/15
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let s = ip.segments();
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        // NAT64 well-known prefix embeds an IPv4 address.
        let [hi, lo] = [s[6], s[7]];
        return is_public_v4(Ipv4Addr::new(
            (hi >> 8) as u8,
            hi as u8,
            (lo >> 8) as u8,
            lo as u8,
        ));
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || s[..6] == [0, 0, 0, 0, 0, 0] // IPv4-compatible and other low space
        || (s[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
        || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        || (s[0] & 0xffc0) == 0xfec0 // fec0::/10 site-local
        || s[0] == 0x2002 // 6to4 embeds an arbitrary IPv4 address
        || (s[0] == 0x2001 && s[1] == 0x0db8)) // documentation
}

/// Marker error placed in the resolver's error chain when a name resolves to a
/// non-public address.
#[derive(Debug)]
struct BlockedAddress;

impl fmt::Display for BlockedAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("destination resolves to a non-public address")
    }
}

impl StdError for BlockedAddress {}

/// Resolver that refuses a name when any of its addresses is non-public, so
/// the connection only ever goes to an address that was checked.
struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let found: Vec<SocketAddr> =
                tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            if found.is_empty() || found.iter().any(|a| !is_public(a.ip())) {
                return Err(Box::new(BlockedAddress) as Box<dyn StdError + Send + Sync>);
            }
            Ok(Box::new(found.into_iter()) as Addrs)
        })
    }
}

/// True when a transport error was caused by [`PublicOnly`] refusing the name.
pub(crate) fn blocked_by_resolver(error: &reqwest::Error) -> bool {
    let mut source: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(e) = source {
        if e.is::<BlockedAddress>() {
            return true;
        }
        source = e.source();
    }
    false
}

/// The two HTTP clients. Built on first use so that a deployment without AI
/// never pays for (or fails on) TLS setup.
#[derive(Default)]
pub(crate) struct Clients {
    public: OnceLock<Option<Client>>,
    private: OnceLock<Option<Client>>,
}

impl Clients {
    pub(crate) fn get(&self, private: bool) -> Result<&Client, AiError> {
        let slot = if private {
            self.private.get_or_init(|| build(true))
        } else {
            self.public.get_or_init(|| build(false))
        };
        slot.as_ref().ok_or_else(|| {
            AiError::new(AiErrorKind::UpstreamUnavailable).with_code("http_client_unavailable")
        })
    }
}

fn build(private: bool) -> Option<Client> {
    let builder = Client::builder()
        .user_agent("tikkit")
        .no_proxy()
        .referer(false)
        .redirect(redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT);
    let builder = if private {
        builder
    } else {
        builder.https_only(true).dns_resolver(Arc::new(PublicOnly))
    };
    builder.build().ok()
}

pub(crate) enum ReadError {
    TooLarge,
    Transport(reqwest::Error),
}

/// Read at most `cap` bytes of the body; anything longer is an error, never a
/// silent truncation.
pub(crate) async fn read_capped(mut response: Response, cap: usize) -> Result<Vec<u8>, ReadError> {
    if response.content_length().is_some_and(|n| n > cap as u64) {
        return Err(ReadError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(ReadError::Transport)? {
        if body.len() + chunk.len() > cap {
            return Err(ReadError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn private_and_special_addresses_are_not_public() {
        for blocked in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "192.0.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "::",
            "::1",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "fec0::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::7f00:1",
            "2002:7f00:1::",
            "2001:db8::1",
            "::7f00:1",
        ] {
            assert!(!is_public(ip(blocked)), "{blocked} must be blocked");
        }
        for allowed in [
            "8.8.8.8",
            "104.18.0.1",
            "2606:4700::1",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ] {
            assert!(is_public(ip(allowed)), "{allowed} must be allowed");
        }
    }

    fn over(url: &str, allow_private_network: bool) -> Endpoint {
        Endpoint::Override {
            base_url: Url::parse(url).unwrap(),
            allow_private_network,
        }
    }

    #[test]
    fn official_endpoint_uses_the_provider_host() {
        let t = target(&Endpoint::Official, "https://api.example.com", "/v1/x").unwrap();
        assert_eq!(t.url.as_str(), "https://api.example.com/v1/x");
        assert!(!t.private);
    }

    #[test]
    fn override_policy() {
        let ok = |e: &Endpoint| target(e, "https://api.example.com", "/v1/x").map(|t| t.url);
        // Public HTTPS override, with a path prefix.
        assert_eq!(
            ok(&over("https://gateway.example.org/ai/", false))
                .unwrap()
                .as_str(),
            "https://gateway.example.org/ai/v1/x"
        );
        // Explicit private policy allows plain HTTP and loopback.
        assert_eq!(
            ok(&over("http://127.0.0.1:9000", true)).unwrap().as_str(),
            "http://127.0.0.1:9000/v1/x"
        );
        for blocked in [
            "http://gateway.example.org",
            "https://127.0.0.1",
            "https://10.0.0.5:8443",
            "https://169.254.169.254",
            "https://[::1]",
            "https://[::ffff:127.0.0.1]",
            "https://user:pw@gateway.example.org",
            "https://gateway.example.org/?x=1",
            "ftp://gateway.example.org",
        ] {
            let err = ok(&over(blocked, false)).unwrap_err();
            assert_eq!(err.kind, AiErrorKind::DestinationBlocked, "{blocked}");
        }
        assert!(ok(&over("ftp://127.0.0.1", true)).is_err());
        assert!(ok(&over("http://user@127.0.0.1", true)).is_err());
    }
}

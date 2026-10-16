//! Authentication: first setup, login/logout, server-side sessions (#9).
//!
//! Public surface for other modules:
//!
//! - [`Actor`] — extractor for the signed-in account (`Option<Actor>` for
//!   optional auth). Reloaded from the database on every request.
//! - [`SessionCookies`] — set/clear the session cookie from a handler.
//! - [`sessions`] — `create`, `revoke_all_for_account`, `rotate_now`, ...
//! - [`password`] — argon2id hashing ([`PasswordHasher`] in
//!   `state.auth.hasher`) and the password policy.
//! - [`username`] — normalization/validation and unique-violation mapping.
//! - [`account`] — `Account`/`SessionInfo` DTOs and `load_account`.
//! - [`audit`] — `audit_events` writer.
//!
//! Roles and account statuses live in [`crate::domain`].

pub mod account;
pub mod actor;
pub mod audit;
pub mod client_ip;
pub mod cookie;
pub mod csrf;
pub mod handlers;
pub mod password;
pub mod rate_limit;
pub mod sessions;
pub mod username;

use std::time::Duration;

pub use account::{Account, AccountTeam, SessionInfo};
pub use actor::{Actor, SessionDetails};
pub use client_ip::ClientIp;
pub use cookie::SessionCookies;
pub use password::{HashParams, PasswordHasher};
pub use rate_limit::RateLimiter;
pub use sessions::{RevokeReason, SessionPolicy};

/// Request budgets for abuse-sensitive endpoints, per `period`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimits {
    pub period: Duration,
    /// Login attempts per client address.
    pub login_per_ip: u32,
    /// Login attempts per client address and normalized username.
    pub login_per_ip_and_username: u32,
    /// First-setup attempts per client address.
    pub setup_per_ip: u32,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self {
            period: Duration::from_secs(60),
            login_per_ip: 50,
            login_per_ip_and_username: 10,
            setup_per_ip: 10,
        }
    }
}

/// Tunable authentication settings. `Default` is the production policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthSettings {
    pub hash_params: HashParams,
    pub session_policy: SessionPolicy,
    pub rate_limits: RateLimits,
}

impl AuthSettings {
    /// Production policy with the cheapest argon2 parameters. **Tests only.**
    pub fn insecure_fast_for_tests() -> Self {
        Self {
            hash_params: HashParams::INSECURE_FAST_FOR_TESTS,
            ..Self::default()
        }
    }
}

/// Process-wide authentication state (in `AppState::auth`).
#[derive(Debug)]
pub struct AuthRuntime {
    pub hasher: PasswordHasher,
    pub session_policy: SessionPolicy,
    pub login_ip_limiter: RateLimiter,
    pub login_username_limiter: RateLimiter,
    pub setup_limiter: RateLimiter,
}

impl AuthRuntime {
    pub fn new(settings: AuthSettings) -> Self {
        let limits = settings.rate_limits;
        Self {
            hasher: PasswordHasher::new(settings.hash_params),
            session_policy: settings.session_policy,
            login_ip_limiter: RateLimiter::new(limits.login_per_ip, limits.period),
            login_username_limiter: RateLimiter::new(
                limits.login_per_ip_and_username,
                limits.period,
            ),
            setup_limiter: RateLimiter::new(limits.setup_per_ip, limits.period),
        }
    }
}

impl Default for AuthRuntime {
    fn default() -> Self {
        Self::new(AuthSettings::default())
    }
}

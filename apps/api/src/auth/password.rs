//! Password hashing (argon2id) and the password policy.
//!
//! Hashes are PHC strings (`$argon2id$v=19$m=…,t=…,p=…$salt$hash`), so the
//! parameters travel with each hash and verification works for hashes made
//! with older parameters. Hashing is CPU and memory heavy: async code must
//! call [`PasswordHasher::hash_blocking`]/[`PasswordHasher::verify_blocking`]
//! (which run on Tokio's blocking pool), never the sync functions directly.

use std::sync::{Arc, OnceLock};

use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{
        PasswordHash, PasswordHasher as _, PasswordVerifier as _, SaltString,
        errors::Error as HashError,
    },
};

use crate::{
    error::{ApiError, FieldError, FieldErrorCode},
    extract::FieldErrors,
};

pub const PASSWORD_MIN_CHARS: usize = 12;
pub const PASSWORD_MAX_CHARS: usize = 256;
/// Inputs longer than this (in bytes) are never hashed; such a login attempt
/// is checked against the dummy hash and fails like any wrong password.
const PASSWORD_MAX_BYTES: usize = PASSWORD_MAX_CHARS * 4;

/// argon2id cost parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HashParams {
    /// Memory in KiB.
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl HashParams {
    /// OWASP Password Storage Cheat Sheet recommendation for argon2id
    /// (m=19 MiB, t=2, p=1). Used in every real deployment.
    pub const RECOMMENDED: Self = Self {
        memory_kib: 19 * 1024,
        iterations: 2,
        parallelism: 1,
    };

    /// The cheapest parameters argon2 accepts. **Tests only**: makes
    /// integration tests fast. Never use for real deployments.
    pub const INSECURE_FAST_FOR_TESTS: Self = Self {
        memory_kib: 8,
        iterations: 1,
        parallelism: 1,
    };
}

impl Default for HashParams {
    fn default() -> Self {
        Self::RECOMMENDED
    }
}

/// Hashes and verifies passwords with fixed argon2id parameters. Cheap to
/// clone (shared dummy hash).
#[derive(Clone)]
pub struct PasswordHasher {
    params: HashParams,
    dummy_hash: Arc<OnceLock<String>>,
}

impl std::fmt::Debug for PasswordHasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasswordHasher")
            .field("params", &self.params)
            .finish_non_exhaustive()
    }
}

impl PasswordHasher {
    pub fn new(params: HashParams) -> Self {
        // Validate eagerly so misconfiguration fails at startup.
        argon2_for(params).expect("valid argon2 parameters");
        Self {
            params,
            dummy_hash: Arc::new(OnceLock::new()),
        }
    }

    pub fn params(&self) -> HashParams {
        self.params
    }

    /// Hashes `password` with a fresh random salt (blocking).
    pub fn hash(&self, password: &str) -> Result<String, HashError> {
        let mut salt = [0u8; 16];
        getrandom::getrandom(&mut salt).map_err(|_| HashError::Crypto)?;
        let salt = SaltString::encode_b64(&salt)?;
        Ok(argon2_for(self.params)?
            .hash_password(password.as_bytes(), &salt)?
            .to_string())
    }

    /// Checks `password` against a stored PHC hash (blocking). Malformed
    /// stored hashes never match.
    pub fn verify(&self, password: &str, stored_hash: &str) -> bool {
        if password.len() > PASSWORD_MAX_BYTES {
            // Still spend comparable time so length does not leak anything.
            let _ = self.verify_against_dummy("");
            return false;
        }
        let Ok(parsed) = PasswordHash::new(stored_hash) else {
            return false;
        };
        // Verification uses the parameters embedded in the hash.
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    }

    /// Runs a full verification against a hash of a random secret, so
    /// requests for unknown or unusable accounts take about as long as real
    /// ones. Always returns `false`.
    pub fn verify_against_dummy(&self, password: &str) -> bool {
        let dummy = self.dummy_hash.get_or_init(|| {
            let mut secret = [0u8; 32];
            let _ = getrandom::getrandom(&mut secret);
            self.hash(&format!("{secret:?}"))
                .expect("hashing a dummy password succeeds")
        });
        let password = if password.len() > PASSWORD_MAX_BYTES {
            ""
        } else {
            password
        };
        let _ = PasswordHash::new(dummy).map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        });
        false
    }

    /// [`Self::hash`] on the blocking pool. Errors are internal errors.
    pub async fn hash_blocking(&self, password: String) -> Result<String, ApiError> {
        let hasher = self.clone();
        tokio::task::spawn_blocking(move || hasher.hash(&password))
            .await
            .map_err(ApiError::internal)?
            .map_err(|e| ApiError::internal(format!("password hashing failed: {e}")))
    }

    /// Verifies on the blocking pool. `stored_hash = None` (unknown or
    /// unusable account) verifies against the dummy hash and returns false.
    pub async fn verify_blocking(
        &self,
        password: String,
        stored_hash: Option<String>,
    ) -> Result<bool, ApiError> {
        let hasher = self.clone();
        tokio::task::spawn_blocking(move || match stored_hash {
            Some(hash) => hasher.verify(&password, &hash),
            None => hasher.verify_against_dummy(&password),
        })
        .await
        .map_err(ApiError::internal)
    }
}

impl Default for PasswordHasher {
    fn default() -> Self {
        Self::new(HashParams::RECOMMENDED)
    }
}

fn argon2_for(params: HashParams) -> Result<Argon2<'static>, HashError> {
    let params = Params::new(
        params.memory_kib,
        params.iterations,
        params.parallelism,
        None,
    )
    .map_err(HashError::from)?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// Password policy for new passwords: 12–256 characters and not equal to the
/// username (compared case-insensitively against both the entered and the
/// normalized username). `field` is the request field name (e.g. `password`
/// or `newPassword`). Violations are pushed onto `errors`.
///
/// Field codes: `required`, `too_short {min}`, `too_long {max}`,
/// `invalid_value {reason: "matches_username"}`.
pub fn check_policy(errors: &mut FieldErrors, field: &str, password: &str, usernames: &[&str]) {
    let len = password.chars().count();
    if len == 0 {
        errors.add(field, FieldErrorCode::Required);
    } else if len < PASSWORD_MIN_CHARS {
        errors.push(
            FieldError::new(field, FieldErrorCode::TooShort).with_param("min", PASSWORD_MIN_CHARS),
        );
    } else if len > PASSWORD_MAX_CHARS {
        errors.push(
            FieldError::new(field, FieldErrorCode::TooLong).with_param("max", PASSWORD_MAX_CHARS),
        );
    } else {
        let lowered = password.trim().to_lowercase();
        if usernames
            .iter()
            .any(|u| !u.trim().is_empty() && lowered == u.trim().to_lowercase())
        {
            errors.push(
                FieldError::new(field, FieldErrorCode::InvalidValue)
                    .with_param("reason", "matches_username"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast() -> PasswordHasher {
        PasswordHasher::new(HashParams::INSECURE_FAST_FOR_TESTS)
    }

    #[test]
    fn hashes_are_argon2id_with_configured_params_and_verify() {
        let hasher = fast();
        let hash = hasher.hash("correct horse battery").unwrap();
        assert!(hash.starts_with("$argon2id$v=19$m=8,t=1,p=1$"), "{hash}");
        assert!(hasher.verify("correct horse battery", &hash));
        assert!(!hasher.verify("correct horse batterY", &hash));
        assert!(!hasher.verify("x", "not a phc string"));
        assert_ne!(hash, hasher.hash("correct horse battery").unwrap());
        assert!(!hasher.verify_against_dummy("anything"));
    }

    #[test]
    fn recommended_params_are_owasp() {
        let hasher = PasswordHasher::default();
        assert_eq!(hasher.params(), HashParams::RECOMMENDED);
        let hash = hasher.hash("correct horse battery").unwrap();
        assert!(
            hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "{hash}"
        );
        // Fast hasher verifies strong hashes (params come from the hash).
        assert!(fast().verify("correct horse battery", &hash));
    }

    #[test]
    fn policy() {
        let codes = |pw: &str| {
            let mut errors = FieldErrors::new();
            check_policy(&mut errors, "password", pw, &["Admin.User", "admin.user"]);
            errors
                .finish()
                .err()
                .map(|e| e.fields().iter().map(|f| f.code).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        assert_eq!(codes(""), vec![FieldErrorCode::Required]);
        assert_eq!(codes("short"), vec![FieldErrorCode::TooShort]);
        assert_eq!(codes(&"x".repeat(257)), vec![FieldErrorCode::TooLong]);
        assert!(codes(&"ä".repeat(256)).is_empty());
        assert_eq!(codes("ADMIN.USER"), vec![FieldErrorCode::TooShort]);
        let mut errors = FieldErrors::new();
        check_policy(
            &mut errors,
            "password",
            "admin.user.1234",
            &["Admin.User.1234"],
        );
        assert_eq!(
            errors.finish().unwrap_err().fields()[0].code,
            FieldErrorCode::InvalidValue
        );
        assert!(codes("a perfectly fine passphrase").is_empty());
    }
}

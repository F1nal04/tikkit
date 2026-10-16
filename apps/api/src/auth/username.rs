//! Username normalization and validation.
//!
//! The login key is `username_normalized`: the input trimmed, NFKC
//! normalized and lowercased. After normalization it must be 3–32
//! characters of `[a-z0-9._-]` (the database enforces the same pattern and
//! uniqueness). The trimmed, as-entered form is kept for display.

use unicode_normalization::UnicodeNormalization;

use crate::{
    error::{ApiError, FieldError, FieldErrorCode},
    extract::FieldErrors,
};

pub const USERNAME_MIN_CHARS: usize = 3;
pub const USERNAME_MAX_CHARS: usize = 32;
/// Upper bound for the stored as-entered form (database CHECK).
const USERNAME_DISPLAY_MAX_CHARS: usize = 64;

/// The normalized login key for `input` (no validation).
pub fn normalize(input: &str) -> String {
    input.trim().nfkc().collect::<String>().to_lowercase()
}

/// A validated username: `display` is the trimmed input, `normalized` the
/// unique login key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Username {
    pub display: String,
    pub normalized: String,
}

/// Validates `input` for account creation, pushing errors for `field`
/// (`required`, `too_short {min}`, `too_long {max}`, `invalid_format`).
pub fn validate(errors: &mut FieldErrors, field: &str, input: &str) -> Option<Username> {
    let display = input.trim();
    let normalized = normalize(input);
    let len = normalized.chars().count();
    if display.is_empty() {
        errors.add(field, FieldErrorCode::Required);
    } else if len < USERNAME_MIN_CHARS {
        errors.push(
            FieldError::new(field, FieldErrorCode::TooShort).with_param("min", USERNAME_MIN_CHARS),
        );
    } else if len > USERNAME_MAX_CHARS || display.chars().count() > USERNAME_DISPLAY_MAX_CHARS {
        errors.push(
            FieldError::new(field, FieldErrorCode::TooLong).with_param("max", USERNAME_MAX_CHARS),
        );
    } else if !normalized
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
    {
        errors.add(field, FieldErrorCode::InvalidFormat);
    } else {
        return Some(Username {
            display: display.to_owned(),
            normalized,
        });
    }
    None
}

/// Maps a violation of the unique normalized-username constraint to
/// `422 validation_failed` with `field`/`already_taken`; any other database
/// error stays an internal error.
pub fn map_unique_violation(error: sqlx::Error, field: &str) -> ApiError {
    let taken = error
        .as_database_error()
        .and_then(|e| e.constraint())
        .is_some_and(|c| c == "accounts_username_normalized_key");
    if taken {
        ApiError::field(field, FieldErrorCode::AlreadyTaken)
    } else {
        ApiError::from(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(input: &str) -> Result<Username, FieldErrorCode> {
        let mut errors = FieldErrors::new();
        let result = validate(&mut errors, "username", input);
        match errors.finish() {
            Ok(()) => Ok(result.unwrap()),
            Err(e) => Err(e.fields()[0].code),
        }
    }

    #[test]
    fn normalizes_with_nfkc_lowercase_and_trim() {
        assert_eq!(normalize("  Ada.Lovelace "), "ada.lovelace");
        // Fullwidth letters and digits fold to ASCII under NFKC.
        assert_eq!(normalize("ＡＤＡ１"), "ada1");
        let u = check(" Grace_Hopper-1 ").unwrap();
        assert_eq!(u.display, "Grace_Hopper-1");
        assert_eq!(u.normalized, "grace_hopper-1");
        assert_eq!(check("ＡＤＡ").unwrap().normalized, "ada");
    }

    #[test]
    fn rejects_invalid_usernames() {
        assert_eq!(check("   "), Err(FieldErrorCode::Required));
        assert_eq!(check("ab"), Err(FieldErrorCode::TooShort));
        assert_eq!(check(&"a".repeat(33)), Err(FieldErrorCode::TooLong));
        assert_eq!(check("ada lovelace"), Err(FieldErrorCode::InvalidFormat));
        assert_eq!(check("müller"), Err(FieldErrorCode::InvalidFormat));
        assert_eq!(check("ada@example"), Err(FieldErrorCode::InvalidFormat));
        assert!(check(&"a".repeat(32)).is_ok());
    }
}

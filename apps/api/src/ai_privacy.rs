//! Input minimization for AI provider calls. See docs/ai-usage.md.
//!
//! The provider may receive only what [`SuggestionRequest`] can hold: category,
//! subject, description, the currently visible conditional answers, and the
//! intake screenshots. [`minimize`] is the last step before such a request
//! leaves the server: it replaces obvious secrets in the requester's text with
//! [`REDACTED`] and cuts every field to the adapter bounds.
//!
//! Redaction is pattern matching over text. It catches secrets that look like
//! secrets (a value after `password:`, a private key block, a provider token);
//! it cannot recognize every secret, and it does not look inside screenshots.

use crate::ai::{
    ConditionalAnswer, MAX_ANSWER_CHARS, MAX_ANSWERS, MAX_CATEGORY_CHARS, MAX_DESCRIPTION_CHARS,
    MAX_LABEL_CHARS, MAX_SUBJECT_CHARS, SuggestionRequest,
};

/// What a redacted secret is replaced with.
pub const REDACTED: &str = "[redacted]";

/// Text with its secrets replaced, and how many replacements were made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    pub redactions: usize,
}

/// A request that is safe to hand to the guarded call.
#[derive(Clone, Debug)]
pub struct Minimized {
    pub request: SuggestionRequest,
    /// Secrets replaced over all text fields. A count only; safe to store.
    pub redactions: usize,
    /// Whether any field or the answer list was cut to its bound.
    pub truncated: bool,
}

/// Redacts and bounds a request built from the submitted intake.
///
/// - `subject`, `description`, and answer values are redacted with
///   [`redact_secrets`]. An answer whose question itself asks for a secret
///   (label "Password", "API key", ...) is replaced entirely.
/// - `category` and answer labels are Admin-defined catalog text and stay as
///   written.
/// - Every text is cut to the adapter bound and the answers to
///   [`MAX_ANSWERS`], so a minimized request never fails the adapters' text
///   limits. Images pass through unchanged; the adapters enforce their limits
///   and never send an image in any other form.
pub fn minimize(raw: SuggestionRequest) -> Minimized {
    let mut redactions = 0;
    let mut truncated = raw.answers.len() > MAX_ANSWERS;
    let mut bound = |text: String, max: usize| match text.char_indices().nth(max) {
        Some((cut, _)) => {
            truncated = true;
            text[..cut].to_owned()
        }
        None => text,
    };
    let mut redact = |text: &str, whole: bool| {
        if whole && !text.trim().is_empty() {
            redactions += 1;
            return REDACTED.to_owned();
        }
        let redacted = redact_secrets(text);
        redactions += redacted.redactions;
        redacted.text
    };

    let category = bound(raw.category, MAX_CATEGORY_CHARS);
    let subject = bound(redact(&raw.subject, false), MAX_SUBJECT_CHARS);
    let description = bound(redact(&raw.description, false), MAX_DESCRIPTION_CHARS);
    let answers = raw
        .answers
        .into_iter()
        .take(MAX_ANSWERS)
        .map(|answer| {
            let value = redact(&answer.value, asks_for_secret(&answer.label));
            ConditionalAnswer {
                label: bound(answer.label, MAX_LABEL_CHARS),
                value: bound(value, MAX_ANSWER_CHARS),
            }
        })
        .collect();
    Minimized {
        request: SuggestionRequest {
            language: raw.language,
            category,
            subject,
            description,
            answers,
            images: raw.images,
        },
        redactions,
        truncated,
    }
}

/// Replaces obvious secrets in `text` with [`REDACTED`]:
///
/// - private key blocks (`-----BEGIN ... PRIVATE KEY-----`);
/// - the value after a secret-naming word: `password: x`, `Passwort = x`,
///   `"api_key":"x"`, `--token=x`, `?token=x`, `my password is x1!`,
///   `Authorization: Bearer x`;
/// - the password in a URL (`scheme://user:password@host`);
/// - tokens in well-known provider formats (`sk-...`, `ghp_...`, `AKIA...`,
///   `xoxb-...`, JWTs) and other long random-looking strings.
///
/// Everything else is returned unchanged, whitespace included.
pub fn redact_secrets(text: &str) -> Redacted {
    let (text, blocks) = redact_key_blocks(text);
    let mut out = String::with_capacity(text.len());
    let mut redactions = blocks;
    let mut pending = Pending::None;
    let mut previous = String::new();
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let space = rest.len() - rest.trim_start().len();
        out.push_str(&rest[..space]);
        rest = &rest[space..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, tail) = rest.split_at(end);
        rest = tail;
        if word.is_empty() {
            continue;
        }
        let (shown, next) = scan_word(word, pending, &previous, &mut redactions);
        out.push_str(&shown);
        pending = next;
        previous = normalize(word);
    }
    Redacted {
        text: out,
        redactions,
    }
}

/// Whether a field label asks for a secret, so its whole answer is one.
fn asks_for_secret(label: &str) -> bool {
    let mut previous = String::new();
    label.split_whitespace().any(|word| {
        let name = normalize(word);
        let hit = names_secret(&name) || names_key(&name, &previous);
        previous = name;
        hit
    })
}

/// What an earlier word decided about the words that follow it.
#[derive(Clone, Copy)]
enum Pending {
    None,
    /// The next word is the secret (`password:` came before).
    Strict,
    /// A secret-naming word came before; the next word is redacted only if it
    /// looks like a secret. `linked`: an "is"-like word has been skipped.
    Loose {
        linked: bool,
    },
    /// Inside a quoted multi-word secret that began with this quote; at most
    /// this many more words belong to it.
    Quoted(char, u8),
}

const KEY_BLOCK_BEGIN: &str = "-----BEGIN ";
const DASHES: &str = "-----";

fn redact_key_blocks(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut count = 0;
    let mut rest = text;
    while let Some(start) = rest.find(KEY_BLOCK_BEGIN) {
        let after = &rest[start + KEY_BLOCK_BEGIN.len()..];
        let is_private = after
            .find(DASHES)
            .is_some_and(|end| after[..end].contains("PRIVATE KEY"));
        if !is_private {
            let keep = start + KEY_BLOCK_BEGIN.len();
            out.push_str(&rest[..keep]);
            rest = &rest[keep..];
            continue;
        }
        out.push_str(&rest[..start]);
        out.push_str(REDACTED);
        count += 1;
        // Through the END line, or to the end of the text when it is missing:
        // a cut-off key is still a key.
        rest = after
            .find("-----END ")
            .and_then(|end| {
                let footer = &after[end + 9..];
                footer
                    .find(DASHES)
                    .map(|close| &footer[close + DASHES.len()..])
            })
            .unwrap_or("");
    }
    out.push_str(rest);
    (out, count)
}

/// Lowercase letters and digits only: `"DB_Password"` and `--db-password` both
/// become `dbpassword`.
fn normalize(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

const KEY_WORDS: [&str; 3] = ["key", "schlüssel", "schluessel"];
/// What makes a "key" a secret: "API key", "Lizenzschlüssel".
const KEY_QUALIFIERS: [&str; 12] = [
    "api",
    "access",
    "secret",
    "private",
    "license",
    "licence",
    "product",
    "recovery",
    "encryption",
    "ssh",
    "lizenz",
    "produkt",
];

/// Whether a normalized word names a secret.
fn names_secret(name: &str) -> bool {
    const SUFFIXES: [&str; 12] = [
        "password",
        "passwort",
        "passwd",
        "pwd",
        "kennwort",
        "passphrase",
        "passcode",
        "secret",
        "token",
        "credential",
        "credentials",
        "authorization",
    ];
    name == "pin"
        || SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
        || KEY_WORDS.iter().any(|key| {
            name.strip_suffix(key)
                .is_some_and(|rest| KEY_QUALIFIERS.iter().any(|q| rest.ends_with(q)))
        })
}

/// "key" as its own word names a secret only after a qualifying word.
fn names_key(name: &str, previous: &str) -> bool {
    KEY_WORDS.contains(&name) && KEY_QUALIFIERS.iter().any(|q| previous.ends_with(q))
}

fn is_link(name: &str) -> bool {
    matches!(
        name,
        "is" | "ist" | "was" | "war" | "lautet" | "heißt" | "heisst"
    )
}

/// A word a person would plausibly have as a password or code: at least four
/// characters with a digit, or a symbol that is not just sentence punctuation.
fn looks_secret(word: &str) -> bool {
    let core = word.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '"', '\'']);
    word.chars().count() >= 4
        && (word.chars().any(|c| c.is_ascii_digit())
            || core.chars().any(|c| "!@#$%^&*+=?_~§".contains(c)))
}

/// Replaces `value` (a whole word or the part after a separator). A value that
/// opens a quote without closing it continues over the following words.
fn redact_value(value: &str, redactions: &mut usize) -> Pending {
    if value != REDACTED {
        *redactions += 1;
    }
    match value.chars().next() {
        Some(quote @ ('"' | '\'')) if !value[1..].contains(quote) => Pending::Quoted(quote, 12),
        _ => Pending::None,
    }
}

/// Handles one whitespace-delimited word. Returns what to output for it and
/// what it means for the next word.
fn scan_word(
    word: &str,
    pending: Pending,
    previous: &str,
    redactions: &mut usize,
) -> (String, Pending) {
    let name = normalize(word);
    let is_separator = matches!(word, ":" | "=" | "=>" | "->" | "-" | "–");
    match pending {
        Pending::Quoted(quote, left) => {
            let closes = word.trim_end_matches(['.', ',', ';', ')']).ends_with(quote);
            let next = if closes || left <= 1 {
                Pending::None
            } else {
                Pending::Quoted(quote, left - 1)
            };
            return (REDACTED.to_owned(), next);
        }
        Pending::Strict if is_separator || matches!(name.as_str(), "bearer" | "basic") => {
            return (word.to_owned(), Pending::Strict);
        }
        Pending::Strict => return (REDACTED.to_owned(), redact_value(word, redactions)),
        Pending::Loose { .. } if matches!(word, ":" | "=") => {
            return (word.to_owned(), Pending::Strict);
        }
        Pending::Loose { linked: false } if is_link(&name) => {
            return (word.to_owned(), Pending::Loose { linked: true });
        }
        Pending::Loose { .. } if looks_secret(word) && !names_secret(&name) => {
            return (REDACTED.to_owned(), redact_value(word, redactions));
        }
        Pending::Loose { .. } | Pending::None => {}
    }

    let word = redact_url_password(word, redactions);
    if let Some(result) = redact_assignments(&word, redactions) {
        return result;
    }
    if names_secret(&name) || names_key(&name, previous) || name == "bearer" {
        return (word, Pending::Loose { linked: false });
    }
    if !word.contains(REDACTED) && is_token(&word) {
        *redactions += 1;
        return (REDACTED.to_owned(), Pending::None);
    }
    (word, Pending::None)
}

/// `scheme://user:password@host` keeps everything but the password.
fn redact_url_password(word: &str, redactions: &mut usize) -> String {
    let Some(scheme) = word.find("://") else {
        return word.to_owned();
    };
    let authority_start = scheme + 3;
    let authority_end = word[authority_start..]
        .find(['/', '?', '#'])
        .map_or(word.len(), |i| authority_start + i);
    let authority = &word[authority_start..authority_end];
    let Some(at) = authority.rfind('@') else {
        return word.to_owned();
    };
    let Some(colon) = authority[..at].find(':') else {
        return word.to_owned();
    };
    if colon + 1 == at {
        return word.to_owned();
    }
    *redactions += 1;
    format!(
        "{}{REDACTED}{}",
        &word[..authority_start + colon + 1],
        &word[authority_start + at..]
    )
}

/// `name=value` and `name:value` inside one word, for every name that names a
/// secret: config lines, JSON, command-line flags, URL query strings. `None`
/// when the word holds no such assignment.
fn redact_assignments(word: &str, redactions: &mut usize) -> Option<(String, Pending)> {
    let mut out = String::new();
    let mut copied = 0;
    let mut pending = Pending::None;
    let mut search = 0;
    while let Some(found) = word[search..].find([':', '=']) {
        let separator = search + found;
        search = separator + 1;
        if separator < copied {
            continue;
        }
        let name_start = word[..separator]
            .rfind(|c: char| !(c.is_alphanumeric() || "_-.\"'".contains(c)))
            .map_or(0, |i| {
                i + word[i..].chars().next().map_or(1, char::len_utf8)
            });
        let name = normalize(&word[name_start..separator]);
        let assigns = names_secret(&name) || (name == "key" && word[separator..].starts_with('='));
        if !assigns {
            continue;
        }
        let value_end = word[search..].find('&').map_or(word.len(), |i| search + i);
        let value = &word[search..value_end];
        out.push_str(&word[copied..search]);
        if value.trim_matches(['"', '\'', ',']).is_empty() {
            // `password:` with the value in the next word.
            out.push_str(value);
            pending = Pending::Strict;
        } else {
            out.push_str(REDACTED);
            pending = redact_value(value, redactions);
        }
        copied = value_end;
        search = value_end;
    }
    if copied == 0 {
        return None;
    }
    out.push_str(&word[copied..]);
    Some((out, pending))
}

/// Whether a word, without surrounding punctuation, is a token in a known
/// provider format or a long random-looking string.
fn is_token(word: &str) -> bool {
    const PREFIXES: [&str; 24] = [
        "sk-",
        "sk_live_",
        "sk_test_",
        "rk_live_",
        "pk_live_",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxs-",
        "AKIA",
        "ASIA",
        "AIza",
        "ya29.",
        "SG.",
        "hf_",
        "npm_",
        "shpat_",
    ];
    let core = word.trim_matches([
        '"', '\'', '(', ')', '[', ']', '<', '>', '{', '}', ',', ';', '.',
    ]);
    let length = core.chars().count();
    let token_chars = |extra: &str| {
        core.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
    };
    let has_digit = core.chars().any(|c| c.is_ascii_digit());
    let has_letter = core.chars().any(|c| c.is_ascii_alphabetic());
    if length >= 20 && token_chars("_-.=+/") && PREFIXES.iter().any(|p| core.starts_with(p)) {
        return true;
    }
    // JSON Web Token: three base64url segments, the first a JSON object.
    if length >= 30
        && core.starts_with("eyJ")
        && core.matches('.').count() == 2
        && token_chars("_-.=")
    {
        return true;
    }
    length >= 32 && token_chars("_-=") && has_digit && has_letter && !is_uuid(core)
}

/// Device and record identifiers are commonly UUIDs and are not secrets.
fn is_uuid(word: &str) -> bool {
    word.len() == 36
        && word.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{ImageInput, ImageMediaType, Language};

    fn redacted(text: &str) -> String {
        redact_secrets(text).text
    }

    #[test]
    fn values_after_a_secret_name_are_redacted() {
        for (input, expected) in [
            ("password: hunter2", "password: [redacted]"),
            (
                "Password = hunter2 and more",
                "Password = [redacted] and more",
            ),
            (
                "Mein Passwort lautet Sommer2024!",
                "Mein Passwort lautet [redacted]",
            ),
            ("my password is Winter#2025.", "my password is [redacted]"),
            ("Kennwort: geheim", "Kennwort: [redacted]"),
            ("WLAN-Passwort: abc", "WLAN-Passwort: [redacted]"),
            ("pwd=abc123", "pwd=[redacted]"),
            ("DB_PASSWORD=p@ss word", "DB_PASSWORD=[redacted] word"),
            ("--token=abcdef rest", "--token=[redacted] rest"),
            (
                r#"{"api_key":"abc123","user":"bob"}"#,
                r#"{"api_key":[redacted]"#,
            ),
            ("API key: 12345", "API key: [redacted]"),
            ("the api key ZX81-22 fails", "the api key [redacted] fails"),
            ("PIN 4821 does not work", "PIN [redacted] does not work"),
            (
                "client_secret: 'two words' x",
                "client_secret: [redacted] [redacted] x",
            ),
            (
                "Authorization: Bearer abc.def.ghi next",
                "Authorization: Bearer [redacted] next",
            ),
            (
                "https://x.test/a?token=abc123&user=5",
                "https://x.test/a?token=[redacted]&user=5",
            ),
            (
                "Lizenz-Schlüssel: ABCD-1234",
                "Lizenz-Schlüssel: [redacted]",
            ),
        ] {
            assert_eq!(redacted(input), expected, "{input}");
        }
    }

    #[test]
    fn ordinary_text_about_passwords_stays_readable() {
        for text in [
            "My password does not work anymore.",
            "I forgot my password and need a reset?",
            "The password is not accepted by Outlook",
            "Passwords are not syncing between devices",
            "Bitte das Passwort zurücksetzen.",
            "Press the key F5: nothing happens",
            "Error 0x80070005 at 12:30, device 3f2b8a1c-9d4e-4f6a-8b7c-1a2b3c4d5e6f",
            "See https://intranet.example.test/help/reset-password for details",
            "Token expired, please sign in again",
            "-----BEGIN CERTIFICATE----- is shown in the dialog",
        ] {
            let result = redact_secrets(text);
            assert_eq!(result.text, text);
            assert_eq!(result.redactions, 0);
        }
    }

    #[test]
    fn private_keys_tokens_and_url_passwords_are_redacted() {
        // Fixtures are assembled with `concat!` so the repository's secret
        // scanner does not take them for real credentials.
        let key = concat!(
            "before\n-----BEGIN OPENSSH PRIVATE",
            " KEY-----\nb3BlbnNzaC1rZXk\nAAAA\n-----END OPENSSH PRIVATE",
            " KEY-----\nafter"
        );
        assert_eq!(redacted(key), "before\n[redacted]\nafter");
        // A cut-off block is redacted to the end.
        assert_eq!(
            redacted("x -----BEGIN RSA PRIVATE KEY-----\nMIIEow"),
            "x [redacted]"
        );
        for token in [
            "sk-proj-abcdefghijklmnop1234",
            "sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "AKIAIOSFODNN7EXAMPLE",
            "xoxb-1234567890-abcdefghijkl",
            concat!("eyJhbGciOiJIUzI1NiJ9.", "eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJl"),
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        ] {
            assert_eq!(
                redacted(&format!("see ({token}), ok")),
                "see [redacted] ok",
                "{token}"
            );
        }
        assert_eq!(
            redacted("mount smb://anna:Geheim123@files.test/share now"),
            "mount smb://anna:[redacted]@files.test/share now"
        );
        assert_eq!(
            redact_secrets("a sk-proj-abcdefghijklmnop1234 b").redactions,
            1
        );
    }

    #[test]
    fn redaction_keeps_whitespace_and_handles_odd_input() {
        assert_eq!(redacted(""), "");
        assert_eq!(redacted("  \n\t "), "  \n\t ");
        assert_eq!(redacted("a\n\n  b\tc "), "a\n\n  b\tc ");
        assert_eq!(redacted("password:"), "password:");
        assert_eq!(
            redacted("password:\n  hunter2\nnext"),
            "password:\n  [redacted]\nnext"
        );
        assert_eq!(redacted("=:=: ::= ="), "=:=: ::= =");
        assert_eq!(redacted("ü=ß: 日本語=パスワード"), "ü=ß: 日本語=パスワード");
        // Already redacted text is stable.
        let once = redacted("password: hunter2 and token=abc");
        assert_eq!(redacted(&once), once);
        // Large input stays linear and intact apart from the secret.
        let big = format!(
            "{} password: x9 {}",
            "word ".repeat(50_000),
            "tail ".repeat(50_000)
        );
        let result = redact_secrets(&big);
        assert_eq!(result.redactions, 1);
        assert_eq!(result.text.len(), big.len() - 2 + REDACTED.len());
    }

    fn request() -> SuggestionRequest {
        SuggestionRequest {
            language: Language::De,
            category: "Software".into(),
            subject: "VPN token: 998877".into(),
            description: "Login fails. password: hunter2".into(),
            answers: vec![
                ConditionalAnswer {
                    label: "Device".into(),
                    value: "Laptop, pwd=abc".into(),
                },
                ConditionalAnswer {
                    label: "Current password".into(),
                    value: "plainwords".into(),
                },
                ConditionalAnswer {
                    label: "API key".into(),
                    value: " ".into(),
                },
            ],
            images: vec![ImageInput {
                media_type: ImageMediaType::Png,
                bytes: vec![1, 2, 3],
            }],
        }
    }

    #[test]
    fn minimize_redacts_requester_text_and_keeps_the_rest() {
        let minimized = minimize(request());
        let r = &minimized.request;
        assert_eq!(r.language, Language::De);
        assert_eq!(r.category, "Software");
        assert_eq!(r.subject, "VPN token: [redacted]");
        assert_eq!(r.description, "Login fails. password: [redacted]");
        assert_eq!(r.answers[0].value, "Laptop, pwd=[redacted]");
        assert_eq!(r.answers[1].label, "Current password");
        assert_eq!(r.answers[1].value, REDACTED);
        assert_eq!(r.answers[2].value, " ");
        assert_eq!(r.images.len(), 1);
        assert_eq!(r.images[0].bytes, vec![1, 2, 3]);
        assert_eq!(minimized.redactions, 4);
        assert!(!minimized.truncated);
    }

    #[test]
    fn minimize_cuts_every_field_to_the_adapter_bounds() {
        let mut raw = request();
        raw.category = "c".repeat(MAX_CATEGORY_CHARS + 1);
        raw.subject = "ß".repeat(MAX_SUBJECT_CHARS + 5);
        raw.description = "d".repeat(MAX_DESCRIPTION_CHARS * 2);
        raw.answers = (0..MAX_ANSWERS + 3)
            .map(|_| ConditionalAnswer {
                label: "l".repeat(MAX_LABEL_CHARS + 1),
                value: "v".repeat(MAX_ANSWER_CHARS + 1),
            })
            .collect();
        let minimized = minimize(raw);
        let r = &minimized.request;
        assert!(minimized.truncated);
        assert_eq!(r.category.chars().count(), MAX_CATEGORY_CHARS);
        assert_eq!(r.subject.chars().count(), MAX_SUBJECT_CHARS);
        assert_eq!(r.description.chars().count(), MAX_DESCRIPTION_CHARS);
        assert_eq!(r.answers.len(), MAX_ANSWERS);
        assert!(r.answers.iter().all(|a| {
            a.label.chars().count() == MAX_LABEL_CHARS
                && a.value.chars().count() == MAX_ANSWER_CHARS
        }));
    }
}

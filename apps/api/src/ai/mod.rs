//! AI provider adapters (OpenAI, Google Gemini, Anthropic). See
//! docs/ai-providers.md.
//!
//! This module only talks to a provider. It takes the provider, model, key and
//! endpoint as plain arguments and knows nothing about stored configuration,
//! tickets, or lifecycle state. One call produces at most one suggestion; there
//! is no conversation, continuation, or regeneration surface.

use std::{fmt, sync::Arc, time::Duration};

use reqwest::{
    Method,
    header::{HeaderMap, HeaderName, HeaderValue, RETRY_AFTER},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;
use url::Url;
use utoipa::ToSchema;

mod anthropic;
mod gemini;
mod http;
mod openai;
mod prompt;

use prompt::{Completion, Prompt};

// --- Limits -----------------------------------------------------------------

/// Input bounds. They are at least as large as what a valid submission can
/// contain, so a real ticket never fails them.
pub const MAX_CATEGORY_CHARS: usize = 200;
pub const MAX_SUBJECT_CHARS: usize = 200;
pub const MAX_DESCRIPTION_CHARS: usize = 20_000;
pub const MAX_ANSWERS: usize = 100;
pub const MAX_LABEL_CHARS: usize = 200;
pub const MAX_ANSWER_CHARS: usize = 2_000;

/// Image bounds, chosen to fit every supported provider at once (Anthropic:
/// 10 MB base64 per image and 8000 px per side; Gemini: 20 MB per request).
pub const MAX_IMAGES: usize = 10;
/// Raw bytes per image (about 6.7 MB once base64-encoded).
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Raw bytes over all images (about 16 MB once base64-encoded).
pub const MAX_TOTAL_IMAGE_BYTES: usize = 12 * 1024 * 1024;
pub const MAX_IMAGE_SIDE: u32 = 8_000;

/// Output bounds.
pub const MAX_OUTPUT_TOKENS: u32 = 8_192;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_EXPLANATION_CHARS: usize = 4_000;
pub const MAX_STEPS: usize = 12;
pub const MAX_STEP_CHARS: usize = 1_000;

const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

// --- Providers and models ----------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[serde(rename = "openai")]
    OpenAi,
    Gemini,
    Anthropic,
}

impl Provider {
    pub const ALL: [Provider; 3] = [Provider::OpenAi, Provider::Gemini, Provider::Anthropic];

    /// Stable machine identifier (`openai`, `gemini`, `anthropic`).
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::OpenAi => "openai",
            Provider::Gemini => "gemini",
            Provider::Anthropic => "anthropic",
        }
    }

    pub fn parse(value: &str) -> Option<Provider> {
        Provider::ALL.into_iter().find(|p| p.as_str() == value)
    }

    fn adapter(self) -> &'static dyn ProviderAdapter {
        match self {
            Provider::OpenAi => &openai::OpenAi,
            Provider::Gemini => &gemini::Gemini,
            Provider::Anthropic => &anthropic::Anthropic,
        }
    }
}

/// A model the adapters are known to work with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub struct ModelInfo {
    pub id: &'static str,
    /// Whether screenshots can be sent to this model as image inputs.
    pub image_input: bool,
}

const fn vision(id: &'static str) -> ModelInfo {
    ModelInfo {
        id,
        image_input: true,
    }
}

// Each list is the provider's current general-purpose text models that support
// structured JSON output. Checked against the provider documentation on
// 2026-11-16; see docs/ai-providers.md before changing.
const OPENAI_MODELS: &[ModelInfo] = &[
    vision("gpt-6-astra"),
    vision("gpt-6.1-sol"),
    vision("gpt-6.1-terra"),
    vision("gpt-6-luna"),
];
const GEMINI_MODELS: &[ModelInfo] = &[
    vision("gemini-4-carbon"),
    vision("gemini-4-argon"),
    vision("gemini-4-hydrogen"),
    vision("gemini-3.8-flash"),
    vision("gemini-3.6-flash"),
    vision("gemini-3.5-flash-lite"),
];
const ANTHROPIC_MODELS: &[ModelInfo] = &[
    vision("claude-opus-5-5"),
    vision("claude-sonnet-5-5"),
    vision("claude-haiku-5-5"),
    vision("claude-opus-5"),
    vision("claude-sonnet-5"),
    vision("claude-opus-4-8"),
    vision("claude-haiku-4-5"),
];

/// The models an Admin may select for `provider`.
pub fn supported_models(provider: Provider) -> &'static [ModelInfo] {
    match provider {
        Provider::OpenAi => OPENAI_MODELS,
        Provider::Gemini => GEMINI_MODELS,
        Provider::Anthropic => ANTHROPIC_MODELS,
    }
}

pub fn model_info(provider: Provider, model: &str) -> Option<&'static ModelInfo> {
    supported_models(provider).iter().find(|m| m.id == model)
}

// --- Call configuration -------------------------------------------------------

/// A provider API key. It has no `Display`, no serialization, and a redacted
/// `Debug`; only the adapters can read the value.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// Where requests go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// The provider's own public API host. The only choice for normal use.
    Official,
    /// An explicit test or deployment override. Without
    /// `allow_private_network` the URL must be HTTPS and resolve to public
    /// addresses only. With it, plain HTTP and loopback/private addresses are
    /// accepted: the operator has declared that destination trusted.
    Override {
        base_url: Url,
        allow_private_network: bool,
    },
}

#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub provider: Provider,
    /// Must be one of [`supported_models`] for `provider`.
    pub model: String,
    pub api_key: ApiKey,
    pub endpoint: Endpoint,
}

/// Timeouts and transport retries for one logical call.
#[derive(Clone, Debug)]
pub struct CallOptions {
    /// Limit for a single HTTP attempt, including reading the response.
    pub request_timeout: Duration,
    /// Limit for the whole call, including retries and backoff.
    pub total_timeout: Duration,
    /// Extra attempts after a transport failure or a 5xx/408 response.
    pub max_retries: u32,
    /// Wait before retry `n` is `retry_backoff * n`.
    pub retry_backoff: Duration,
}

impl Default for CallOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(60),
            total_timeout: Duration::from_secs(120),
            max_retries: 2,
            retry_backoff: Duration::from_secs(1),
        }
    }
}

/// Cooperative cancellation for a running call. Dropping the call's future
/// cancels it as well; this handle exists so another task can stop it and the
/// caller still receives [`AiErrorKind::Cancelled`].
#[derive(Clone)]
pub struct Cancel(Arc<watch::Sender<bool>>);

impl Cancel {
    pub fn new() -> Self {
        Self(Arc::new(watch::Sender::new(false)))
    }

    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    pub async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        if rx.wait_for(|cancelled| *cancelled).await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

impl Default for Cancel {
    fn default() -> Self {
        Self::new()
    }
}

// --- Request and result -------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    En,
    De,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageMediaType {
    Png,
    Jpeg,
    WebP,
}

impl ImageMediaType {
    pub fn as_str(self) -> &'static str {
        match self {
            ImageMediaType::Png => "image/png",
            ImageMediaType::Jpeg => "image/jpeg",
            ImageMediaType::WebP => "image/webp",
        }
    }

    pub fn parse(value: &str) -> Option<ImageMediaType> {
        [Self::Png, Self::Jpeg, Self::WebP]
            .into_iter()
            .find(|m| m.as_str() == value)
    }
}

/// One screenshot, as stored (already validated and re-encoded by
/// `screenshots`). It is sent as a real image input or the call fails.
#[derive(Clone)]
pub struct ImageInput {
    pub media_type: ImageMediaType,
    pub bytes: Vec<u8>,
}

/// A visible conditional answer: the field label and the requester's answer.
#[derive(Clone)]
pub struct ConditionalAnswer {
    pub label: String,
    pub value: String,
}

/// The requester context for one suggestion. Every text field and every image
/// is untrusted data; none of it reaches the system instructions.
#[derive(Clone)]
pub struct SuggestionRequest {
    /// Language the solution must be written in.
    pub language: Language,
    pub category: String,
    /// May be empty.
    pub subject: String,
    pub description: String,
    pub answers: Vec<ConditionalAnswer>,
    pub images: Vec<ImageInput>,
}

// Ticket content must not reach logs, so `Debug` shows sizes only.
impl fmt::Debug for SuggestionRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SuggestionRequest")
            .field("language", &self.language)
            .field("description_chars", &self.description.chars().count())
            .field("answers", &self.answers.len())
            .field("images", &self.images.len())
            .finish()
    }
}

/// Token counts as reported by the provider. `None` means the provider did not
/// report that value; it is never estimated or summed locally.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

/// The one proposed solution: explanatory text plus ordered steps (1 to
/// [`MAX_STEPS`]). All strings are trimmed, non-empty, and within the bounds.
#[derive(Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub explanation: String,
    pub steps: Vec<String>,
    pub usage: Usage,
}

impl fmt::Debug for Suggestion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Suggestion")
            .field("explanation_chars", &self.explanation.chars().count())
            .field("steps", &self.steps.len())
            .field("usage", &self.usage)
            .finish()
    }
}

// --- Errors -------------------------------------------------------------------

/// Every failure is one of these. None of them is fatal for the ticket: the
/// caller records the kind and hands the ticket to human support.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AiErrorKind {
    /// The request exceeds an input bound or is missing required text. No
    /// provider call was made.
    InvalidInput,
    /// The provider rejected the credential (401/403, or an invalid-key error).
    InvalidCredentials,
    /// Rate limit or quota (429/402).
    RateLimited,
    /// The provider rejected the request or model, or the model refused,
    /// filtered, or failed the generation. Also: model not in the supported
    /// list (no call made).
    ModelError,
    /// An attempt or the whole call exceeded its time limit.
    Timeout,
    /// Connection failure or 5xx/408 after the bounded retries.
    UpstreamUnavailable,
    /// The response is not the expected single suggestion.
    MalformedOutput,
    /// The response body or the suggestion exceeds an output bound, or the
    /// model ran out of output tokens.
    OversizedOutput,
    /// Images cannot be sent or were rejected: model without image input, an
    /// image outside the bounds, or a provider error about an image.
    Vision,
    /// The destination is not allowed, or the provider answered with a
    /// redirect (redirects are never followed).
    DestinationBlocked,
    /// Stopped through [`Cancel`].
    Cancelled,
}

impl AiErrorKind {
    /// Stable machine identifier for storage and reporting.
    pub fn as_str(self) -> &'static str {
        match self {
            AiErrorKind::InvalidInput => "invalid_input",
            AiErrorKind::InvalidCredentials => "invalid_credentials",
            AiErrorKind::RateLimited => "rate_limited",
            AiErrorKind::ModelError => "model_error",
            AiErrorKind::Timeout => "timeout",
            AiErrorKind::UpstreamUnavailable => "upstream_unavailable",
            AiErrorKind::MalformedOutput => "malformed_output",
            AiErrorKind::OversizedOutput => "oversized_output",
            AiErrorKind::Vision => "vision_failed",
            AiErrorKind::DestinationBlocked => "destination_blocked",
            AiErrorKind::Cancelled => "cancelled",
        }
    }
}

/// A provider-call failure. It never carries the key, the request, a URL, or
/// the provider's free-text message, so it is safe to log and store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiError {
    pub kind: AiErrorKind,
    /// HTTP status of the provider response, when there was one.
    pub status: Option<u16>,
    /// Short identifier: the provider's own error code/type (restricted to
    /// `[A-Za-z0-9_.-]`, at most 64 characters) or a local reason such as
    /// `image_too_large`.
    pub provider_code: Option<String>,
    /// The provider's `Retry-After`, in seconds, when it sent one.
    pub retry_after: Option<Duration>,
}

impl AiError {
    pub(crate) fn new(kind: AiErrorKind) -> Self {
        Self {
            kind,
            status: None,
            provider_code: None,
            retry_after: None,
        }
    }

    pub(crate) fn with_code(mut self, code: &str) -> Self {
        self.provider_code = sanitize_code(code);
        self
    }
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AI provider call failed: {}", self.kind.as_str())?;
        if let Some(status) = self.status {
            write!(f, " (HTTP {status})")?;
        }
        Ok(())
    }
}

impl std::error::Error for AiError {}

fn sanitize_code(code: &str) -> Option<String> {
    let ok = !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
    ok.then(|| code.to_owned())
}

// --- Provider interface -------------------------------------------------------

/// What a provider has to supply. Everything vendor-specific (paths, auth
/// headers, payload shape, response shape, error codes) lives behind this
/// trait; transport, limits, retries, and output validation are shared.
pub(crate) trait ProviderAdapter: Sync {
    /// Official API origin, e.g. `https://api.openai.com`.
    fn base_url(&self) -> &'static str;
    /// Authentication and version headers.
    fn headers(&self, api_key: &str) -> Vec<(&'static str, String)>;
    fn suggestion_path(&self) -> &'static str;
    fn suggestion_body(&self, model: &str, prompt: &Prompt) -> Value;
    /// Extract the text and usage from a 2xx response body.
    fn parse_suggestion(&self, body: &Value) -> Result<Completion, AiError>;
    /// A GET that verifies the key and the model without generating anything.
    fn probe_path(&self, model: &str) -> String;
    /// The provider's error identifier from a non-2xx body.
    fn error_code(&self, body: &Value) -> Option<String>;
    /// Provider-specific exceptions to the status mapping in [`classify`].
    fn error_kind(&self, _status: u16, _body: &Value) -> Option<AiErrorKind> {
        None
    }
}

fn classify(status: u16) -> AiErrorKind {
    match status {
        401 | 403 => AiErrorKind::InvalidCredentials,
        402 | 429 => AiErrorKind::RateLimited,
        408 | 500..=599 => AiErrorKind::UpstreamUnavailable,
        300..=399 => AiErrorKind::DestinationBlocked,
        _ => AiErrorKind::ModelError,
    }
}

pub(crate) fn u64_at(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

// --- Client -------------------------------------------------------------------

/// Entry point for provider calls. Cheap to clone; create one per process.
#[derive(Clone, Default)]
pub struct AiClient {
    options: CallOptions,
    clients: Arc<http::Clients>,
}

struct Call<'a> {
    config: &'a ProviderConfig,
    method: Method,
    path: &'a str,
    body: Option<Vec<u8>>,
    has_images: bool,
    request_timeout: Duration,
    max_retries: u32,
}

impl AiClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_options(options: CallOptions) -> Self {
        Self {
            options,
            clients: Arc::default(),
        }
    }

    /// Ask the configured model for one solution proposal.
    ///
    /// Transport retries stay inside this call and can never yield more than
    /// one suggestion. Any error means no usable suggestion exists.
    pub async fn suggest(
        &self,
        config: &ProviderConfig,
        request: &SuggestionRequest,
        cancel: &Cancel,
    ) -> Result<Suggestion, AiError> {
        if cancel.is_cancelled() {
            return Err(AiError::new(AiErrorKind::Cancelled));
        }
        let model = model_info(config.provider, &config.model)
            .ok_or_else(|| AiError::new(AiErrorKind::ModelError).with_code("unsupported_model"))?;
        if !request.images.is_empty() && !model.image_input {
            return Err(AiError::new(AiErrorKind::Vision).with_code("model_without_image_input"));
        }
        let prompt = prompt::build(request)?;
        let adapter = config.provider.adapter();
        let body = serde_json::to_vec(&adapter.suggestion_body(model.id, &prompt))
            .map_err(|_| AiError::new(AiErrorKind::InvalidInput))?;
        let call = Call {
            config,
            method: Method::POST,
            path: adapter.suggestion_path(),
            body: Some(body),
            has_images: !prompt.images.is_empty(),
            request_timeout: self.options.request_timeout,
            max_retries: self.options.max_retries,
        };
        let run = async {
            let response = self.execute(call).await?;
            prompt::parse(adapter.parse_suggestion(&response)?)
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(AiError::new(AiErrorKind::Cancelled)),
            result = tokio::time::timeout(self.options.total_timeout, run) => {
                result.unwrap_or_else(|_| Err(AiError::new(AiErrorKind::Timeout)))
            }
        }
    }

    /// Connection test: checks that the key is accepted and the model exists.
    /// It reads model metadata only, so it generates nothing and costs no
    /// tokens. One attempt, short timeout.
    pub async fn probe(&self, config: &ProviderConfig) -> Result<(), AiError> {
        let model = model_info(config.provider, &config.model)
            .ok_or_else(|| AiError::new(AiErrorKind::ModelError).with_code("unsupported_model"))?;
        let path = config.provider.adapter().probe_path(model.id);
        let call = Call {
            config,
            method: Method::GET,
            path: &path,
            body: None,
            has_images: false,
            request_timeout: self.options.request_timeout.min(PROBE_TIMEOUT),
            max_retries: 0,
        };
        self.execute(call).await.map(|_| ())
    }

    async fn execute(&self, call: Call<'_>) -> Result<Value, AiError> {
        let adapter = call.config.provider.adapter();
        let target = http::target(&call.config.endpoint, adapter.base_url(), call.path)?;
        let client = self.clients.get(target.private)?;

        let mut headers = HeaderMap::new();
        for (name, value) in adapter.headers(&call.config.api_key.0) {
            // A key that is not a valid header value can never authenticate.
            let mut value = HeaderValue::from_str(&value)
                .map_err(|_| AiError::new(AiErrorKind::InvalidCredentials))?;
            value.set_sensitive(true);
            headers.insert(HeaderName::from_static(name), value);
        }

        let mut attempt = 0;
        loop {
            let mut request = client
                .request(call.method.clone(), target.url.clone())
                .headers(headers.clone())
                .timeout(call.request_timeout);
            if let Some(body) = &call.body {
                request = request
                    .header("content-type", "application/json")
                    .body(body.clone());
            }
            let error = match self.attempt(request, adapter, call.has_images).await {
                Ok(value) => return Ok(value),
                Err(error) => error,
            };
            if error.kind != AiErrorKind::UpstreamUnavailable || attempt >= call.max_retries {
                return Err(error);
            }
            attempt += 1;
            tokio::time::sleep(self.options.retry_backoff * attempt).await;
        }
    }

    async fn attempt(
        &self,
        request: reqwest::RequestBuilder,
        adapter: &dyn ProviderAdapter,
        has_images: bool,
    ) -> Result<Value, AiError> {
        let transport = |e: reqwest::Error| {
            AiError::new(if e.is_timeout() {
                AiErrorKind::Timeout
            } else if http::blocked_by_resolver(&e) {
                AiErrorKind::DestinationBlocked
            } else {
                AiErrorKind::UpstreamUnavailable
            })
        };
        let response = request.send().await.map_err(transport)?;
        let status = response.status();

        if status.is_success() {
            let body = match http::read_capped(response, MAX_RESPONSE_BYTES).await {
                Ok(body) => body,
                Err(http::ReadError::TooLarge) => {
                    return Err(AiError::new(AiErrorKind::OversizedOutput));
                }
                Err(http::ReadError::Transport(e)) => return Err(transport(e)),
            };
            return serde_json::from_slice(&body).map_err(|_| {
                let mut error = AiError::new(AiErrorKind::MalformedOutput);
                error.status = Some(status.as_u16());
                error
            });
        }

        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        // The error body is only used to pick a kind and a short code. Its
        // message can echo request content or parts of the key, so it is
        // never kept.
        let body: Value = http::read_capped(response, MAX_ERROR_BODY_BYTES)
            .await
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        let code = status.as_u16();
        let mut kind = adapter
            .error_kind(code, &body)
            .unwrap_or_else(|| classify(code));
        if kind == AiErrorKind::ModelError && has_images && mentions_image(&body) {
            kind = AiErrorKind::Vision;
        }
        Err(AiError {
            kind,
            status: Some(code),
            provider_code: adapter.error_code(&body).as_deref().and_then(sanitize_code),
            retry_after,
        })
    }
}

/// Whether a provider's rejection of a request that carried images is about
/// the images. All three providers put the reason in `error.message`.
fn mentions_image(body: &Value) -> bool {
    body.pointer("/error/message")
        .and_then(Value::as_str)
        .is_some_and(|m| {
            let m = m.to_ascii_lowercase();
            m.contains("image") || m.contains("vision")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_and_content_are_not_in_debug_output() {
        let config = ProviderConfig {
            provider: Provider::Anthropic,
            model: "claude-haiku-5-5".into(),
            api_key: ApiKey::new("super-secret-value"),
            endpoint: Endpoint::Official,
        };
        let shown = format!("{config:?}");
        assert!(!shown.contains("super-secret-value"));
        assert!(shown.contains("<redacted>"));

        let request = SuggestionRequest {
            language: Language::En,
            category: "Hardware".into(),
            subject: "private subject".into(),
            description: "private description".into(),
            answers: vec![],
            images: vec![],
        };
        assert!(!format!("{request:?}").contains("private"));
        let suggestion = Suggestion {
            explanation: "private text".into(),
            steps: vec!["private step".into()],
            usage: Usage::default(),
        };
        assert!(!format!("{suggestion:?}").contains("private"));
    }

    #[test]
    fn identifiers_are_stable() {
        for provider in Provider::ALL {
            assert_eq!(Provider::parse(provider.as_str()), Some(provider));
            assert_eq!(
                serde_json::to_value(provider).unwrap(),
                provider.as_str(),
                "serde and as_str agree"
            );
            assert!(!supported_models(provider).is_empty());
        }
        assert_eq!(Provider::parse("mistral"), None);
        assert!(model_info(Provider::OpenAi, "claude-opus-5-5").is_none());
        assert!(
            model_info(Provider::Anthropic, "claude-opus-5-5")
                .unwrap()
                .image_input
        );
    }

    #[test]
    fn official_hosts() {
        assert_eq!(
            Provider::OpenAi.adapter().base_url(),
            "https://api.openai.com"
        );
        assert_eq!(
            Provider::Gemini.adapter().base_url(),
            "https://generativelanguage.googleapis.com"
        );
        assert_eq!(
            Provider::Anthropic.adapter().base_url(),
            "https://api.anthropic.com"
        );
    }

    #[test]
    fn provider_codes_are_sanitized() {
        assert_eq!(
            sanitize_code("rate_limit_error").unwrap(),
            "rate_limit_error"
        );
        assert!(sanitize_code("Incorrect API key provided: abc").is_none());
        assert!(sanitize_code(&"x".repeat(65)).is_none());
        assert!(sanitize_code("").is_none());
    }
}

//! Google Gemini adapter: Interactions API (`POST /v1beta/interactions`),
//! `x-goog-api-key` authentication, images as inline `image` content, JSON
//! output through `response_format`.

use serde_json::{Value, json};

use super::{
    AiError, AiErrorKind, ProviderAdapter, Usage,
    prompt::{Completion, Prompt},
    u64_at,
};

const API_REVISION: &str = "2026-05-20";

pub(crate) struct Gemini;

impl ProviderAdapter for Gemini {
    fn base_url(&self) -> &'static str {
        "https://generativelanguage.googleapis.com"
    }

    fn headers(&self, api_key: &str) -> Vec<(&'static str, String)> {
        vec![
            ("x-goog-api-key", api_key.to_owned()),
            ("api-revision", API_REVISION.to_owned()),
        ]
    }

    fn suggestion_path(&self) -> &'static str {
        "/v1beta/interactions"
    }

    fn suggestion_body(&self, model: &str, prompt: &Prompt) -> Value {
        let mut input = vec![json!({ "type": "text", "text": prompt.user_text })];
        input.extend(prompt.images.iter().map(
            |image| json!({ "type": "image", "data": image.data, "mime_type": image.media_type }),
        ));
        json!({
            "model": model,
            "system_instruction": prompt.system,
            "input": input,
            "generation_config": {
                "max_output_tokens": prompt.max_output_tokens,
                "thinking_level": "low",
            },
            "response_format": {
                "type": "text",
                "mime_type": "application/json",
                "schema": prompt.schema,
            },
            // Stateless: the interaction is not kept for a later turn.
            "store": false,
        })
    }

    fn parse_suggestion(&self, body: &Value) -> Result<Completion, AiError> {
        match body.get("status").and_then(Value::as_str) {
            Some("completed") => {}
            // "Completed, but contains incomplete results (e.g. hitting max_tokens)."
            Some("incomplete") => return Err(AiError::new(AiErrorKind::OversizedOutput)),
            Some(
                other @ ("failed" | "cancelled" | "requires_action" | "requires_continuation"),
            ) => {
                return Err(AiError::new(AiErrorKind::ModelError).with_code(other));
            }
            _ => return Err(AiError::new(AiErrorKind::MalformedOutput)),
        }
        let mut text = String::new();
        let steps = body.get("steps").and_then(Value::as_array);
        for step in steps.into_iter().flatten() {
            if step.get("type").and_then(Value::as_str) != Some("model_output") {
                continue; // thought steps carry no answer text
            }
            let parts = step.get("content").and_then(Value::as_array);
            for part in parts.into_iter().flatten() {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    text.push_str(part.get("text").and_then(Value::as_str).unwrap_or_default());
                }
            }
        }
        if text.is_empty() {
            return Err(AiError::new(AiErrorKind::MalformedOutput));
        }
        let usage = body.get("usage").unwrap_or(&Value::Null);
        Ok(Completion {
            text,
            usage: Usage {
                input_tokens: u64_at(usage, "total_input_tokens"),
                output_tokens: u64_at(usage, "total_output_tokens"),
                total_tokens: u64_at(usage, "total_tokens"),
            },
        })
    }

    fn probe_path(&self, model: &str) -> String {
        format!("/v1beta/models/{model}")
    }

    fn error_code(&self, body: &Value) -> Option<String> {
        // Google errors carry `error.status` (e.g. `RESOURCE_EXHAUSTED`); the
        // Interactions API can also send a string `error.code`.
        let error = body.get("error")?;
        ["status", "code"]
            .into_iter()
            .find_map(|key| error.get(key).and_then(Value::as_str))
            .map(str::to_owned)
    }

    fn error_kind(&self, status: u16, body: &Value) -> Option<AiErrorKind> {
        // An invalid key is reported as 400 with reason `API_KEY_INVALID`, not 401.
        let details = body.pointer("/error/details").and_then(Value::as_array);
        let invalid_key = details
            .into_iter()
            .flatten()
            .any(|detail| detail.get("reason").and_then(Value::as_str) == Some("API_KEY_INVALID"));
        (status == 400 && invalid_key).then_some(AiErrorKind::InvalidCredentials)
    }
}

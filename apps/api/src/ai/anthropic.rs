//! Anthropic adapter: Messages API (`POST /v1/messages`), `x-api-key`
//! authentication, images as base64 `image` blocks, JSON output through
//! `output_config.format`.

use serde_json::{Value, json};

use super::{
    AiError, AiErrorKind, ProviderAdapter, Usage,
    prompt::{Completion, Prompt},
    u64_at,
};

const API_VERSION: &str = "2026-10-21";
/// The one listed model that does not accept `output_config.effort`.
const NO_EFFORT_MODEL: &str = "claude-haiku-4-5";

pub(crate) struct Anthropic;

impl ProviderAdapter for Anthropic {
    fn base_url(&self) -> &'static str {
        "https://api.anthropic.com"
    }

    fn headers(&self, api_key: &str) -> Vec<(&'static str, String)> {
        vec![
            ("x-api-key", api_key.to_owned()),
            ("anthropic-version", API_VERSION.to_owned()),
        ]
    }

    fn suggestion_path(&self) -> &'static str {
        "/v1/messages"
    }

    fn suggestion_body(&self, model: &str, prompt: &Prompt) -> Value {
        // Images go before the text, as the vision guide recommends.
        let mut content: Vec<Value> = prompt
            .images
            .iter()
            .map(|image| {
                json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": image.media_type,
                        "data": image.data,
                    },
                })
            })
            .collect();
        content.push(json!({ "type": "text", "text": prompt.user_text }));
        let mut output_config = json!({
            "format": { "type": "json_schema", "schema": prompt.schema }
        });
        // Thinking counts against `max_tokens` and is on by default on the
        // newer models, so pin it low like the other adapters do. Haiku 4.5
        // rejects the parameter.
        if model != NO_EFFORT_MODEL {
            output_config["effort"] = json!("low");
        }
        json!({
            "model": model,
            "max_tokens": prompt.max_output_tokens,
            "system": prompt.system,
            "messages": [{ "role": "user", "content": content }],
            "output_config": output_config,
        })
    }

    fn parse_suggestion(&self, body: &Value) -> Result<Completion, AiError> {
        match body.get("stop_reason").and_then(Value::as_str) {
            Some("end_turn" | "stop_sequence") => {}
            Some("max_tokens" | "model_context_window_exceeded") => {
                return Err(AiError::new(AiErrorKind::OversizedOutput));
            }
            Some("refusal") => {
                return Err(AiError::new(AiErrorKind::ModelError).with_code("refusal"));
            }
            // `tool_use` / `pause_turn` cannot be a finished answer: no tools are offered.
            _ => return Err(AiError::new(AiErrorKind::MalformedOutput)),
        }
        let mut text = String::new();
        let blocks = body.get("content").and_then(Value::as_array);
        for block in blocks.into_iter().flatten() {
            // Thinking blocks are skipped; only text blocks hold the answer.
            if block.get("type").and_then(Value::as_str) == Some("text") {
                text.push_str(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
            }
        }
        if text.is_empty() {
            return Err(AiError::new(AiErrorKind::MalformedOutput));
        }
        let usage = body.get("usage").unwrap_or(&Value::Null);
        Ok(Completion {
            text,
            usage: Usage {
                input_tokens: u64_at(usage, "input_tokens"),
                output_tokens: u64_at(usage, "output_tokens"),
                // The Messages API reports no total.
                total_tokens: None,
            },
        })
    }

    fn probe_path(&self, model: &str) -> String {
        format!("/v1/models/{model}")
    }

    fn error_code(&self, body: &Value) -> Option<String> {
        body.pointer("/error/type")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

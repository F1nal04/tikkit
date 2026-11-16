//! OpenAI adapter: Responses API (`POST /v1/responses`), bearer authentication,
//! images as `input_image` data URLs, strict JSON-schema output.

use serde_json::{Value, json};

use super::{
    AiError, AiErrorKind, ProviderAdapter, Usage,
    prompt::{Completion, Prompt},
    u64_at,
};

pub(crate) struct OpenAi;

impl ProviderAdapter for OpenAi {
    fn base_url(&self) -> &'static str {
        "https://api.openai.com"
    }

    fn headers(&self, api_key: &str) -> Vec<(&'static str, String)> {
        vec![("authorization", format!("Bearer {api_key}"))]
    }

    fn suggestion_path(&self) -> &'static str {
        "/v1/responses"
    }

    fn suggestion_body(&self, model: &str, prompt: &Prompt) -> Value {
        let mut content = vec![json!({ "type": "input_text", "text": prompt.user_text })];
        content.extend(prompt.images.iter().map(|image| {
            json!({
                "type": "input_image",
                "image_url": format!("data:{};base64,{}", image.media_type, image.data),
                "detail": "auto",
            })
        }));
        json!({
            "model": model,
            "instructions": prompt.system,
            "input": [{ "role": "user", "content": content }],
            "max_output_tokens": prompt.max_output_tokens,
            "reasoning": { "effort": "low" },
            // Stateless: nothing is kept for a later turn.
            "store": false,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "ticket_suggestion",
                    "strict": true,
                    "schema": prompt.schema,
                }
            },
        })
    }

    fn parse_suggestion(&self, body: &Value) -> Result<Completion, AiError> {
        match body.get("status").and_then(Value::as_str) {
            Some("completed") => {}
            Some("incomplete") => {
                let reason = body
                    .pointer("/incomplete_details/reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                return Err(if reason == "max_output_tokens" {
                    AiError::new(AiErrorKind::OversizedOutput)
                } else {
                    AiError::new(AiErrorKind::ModelError).with_code(reason)
                });
            }
            Some("failed") => {
                let code = body
                    .pointer("/error/code")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                return Err(AiError::new(AiErrorKind::ModelError).with_code(code));
            }
            _ => return Err(AiError::new(AiErrorKind::MalformedOutput)),
        }
        let mut text = String::new();
        let items = body.get("output").and_then(Value::as_array);
        for item in items.into_iter().flatten() {
            if item.get("type").and_then(Value::as_str) != Some("message") {
                continue; // reasoning items carry no answer text
            }
            let parts = item.get("content").and_then(Value::as_array);
            for part in parts.into_iter().flatten() {
                match part.get("type").and_then(Value::as_str) {
                    Some("output_text") => {
                        text.push_str(part.get("text").and_then(Value::as_str).unwrap_or_default());
                    }
                    Some("refusal") => {
                        return Err(AiError::new(AiErrorKind::ModelError).with_code("refusal"));
                    }
                    _ => {}
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
                input_tokens: u64_at(usage, "input_tokens"),
                output_tokens: u64_at(usage, "output_tokens"),
                total_tokens: u64_at(usage, "total_tokens"),
            },
        })
    }

    fn probe_path(&self, model: &str) -> String {
        format!("/v1/models/{model}")
    }

    fn error_code(&self, body: &Value) -> Option<String> {
        let error = body.get("error")?;
        ["code", "type"]
            .into_iter()
            .find_map(|key| error.get(key).and_then(Value::as_str))
            .map(str::to_owned)
    }
}

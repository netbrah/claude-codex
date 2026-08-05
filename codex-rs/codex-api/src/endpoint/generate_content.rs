//! HTTP endpoint client for Google Gemini `streamGenerateContent`.

use crate::auth::SharedAuthProvider;
use crate::common::ResponseStream;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::sse::spawn_generate_content_stream_from_response;
use codex_client::HttpTransport;
use codex_client::RequestTelemetry;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use serde::Serialize;
use std::sync::Arc;
use tracing::instrument;

use crate::endpoint::session::EndpointSession;

/// Request body for Gemini `generateContent` / `streamGenerateContent`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentRequest {
    pub contents: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_settings: Option<Vec<SafetySetting>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<serde_json::Value>,
    /// Gemini thinking configuration (thinkingBudget). Only included
    /// for models that support thinking (gemini-2.5+).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<serde_json::Value>,
}

/// Generation parameters for Gemini.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
}

/// Per-request safety setting.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetySetting {
    pub category: String,
    pub threshold: String,
}

pub struct GenerateContentClient<T: HttpTransport> {
    session: EndpointSession<T>,
    model: String,
}

impl<T: HttpTransport> GenerateContentClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider, model: String) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
            model,
        }
    }

    pub fn with_telemetry(self, request: Option<Arc<dyn RequestTelemetry>>) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
            ..self
        }
    }

    #[instrument(
        name = "generate_content.stream_request",
        level = "info",
        skip_all,
        fields(
            transport = "generate_content_http",
            http.method = "POST",
            api.path = "streamGenerateContent"
        )
    )]
    pub async fn stream_request(
        &self,
        request: GenerateContentRequest,
        extra_headers: HeaderMap,
    ) -> Result<ResponseStream, ApiError> {
        let body = serde_json::to_value(&request).map_err(|e| {
            ApiError::Stream(format!("failed to encode generateContent request: {e}"))
        })?;

        let mut headers = extra_headers;
        headers.insert(
            http::header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );

        let stream_response = self
            .session
            .stream_with(Method::POST, &self.path(), headers, Some(body), |_| {})
            .await?;

        Ok(spawn_generate_content_stream_from_response(
            stream_response,
            self.session.provider().stream_idle_timeout,
        ))
    }

    /// Path for the streaming endpoint.
    ///
    /// Gemini uses model-specific paths:
    /// `models/{model}:streamGenerateContent?alt=sse`
    fn path(&self) -> String {
        format!("models/{}:streamGenerateContent?alt=sse", self.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_correctly() {
        let request = GenerateContentRequest {
            contents: vec![serde_json::json!({
                "role": "user",
                "parts": [{"text": "Hello"}]
            })],
            system_instruction: Some(serde_json::json!({
                "parts": [{"text": "You are helpful."}]
            })),
            generation_config: Some(GenerationConfig {
                temperature: Some(0.7),
                top_p: None,
                top_k: None,
                max_output_tokens: Some(8192),
                stop_sequences: None,
                presence_penalty: None,
                frequency_penalty: None,
                seed: None,
            }),
            safety_settings: None,
            tools: None,
            tool_config: None,
            thinking_config: None,
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["contents"][0]["role"], "user");
        assert_eq!(
            json["systemInstruction"]["parts"][0]["text"],
            "You are helpful."
        );
        assert_eq!(json["generationConfig"]["temperature"], 0.7);
        assert_eq!(json["generationConfig"]["maxOutputTokens"], 8192);
        assert!(json.get("safetySettings").is_none());
    }

    #[test]
    fn path_includes_model() {
        let path = format!(
            "models/{}:streamGenerateContent?alt=sse",
            "gemini-2.5-flash"
        );
        assert_eq!(
            path,
            "models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn generation_config_omits_none_fields() {
        let config = GenerationConfig {
            temperature: None,
            top_p: None,
            top_k: None,
            max_output_tokens: None,
            stop_sequences: None,
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
        };
        let json = serde_json::to_value(&config).unwrap();
        assert!(json.as_object().unwrap().is_empty());
    }

    #[test]
    fn safety_settings_serialize() {
        let settings = vec![SafetySetting {
            category: "HARM_CATEGORY_HARASSMENT".to_string(),
            threshold: "BLOCK_MEDIUM_AND_ABOVE".to_string(),
        }];
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json[0]["category"], "HARM_CATEGORY_HARASSMENT");
        assert_eq!(json[0]["threshold"], "BLOCK_MEDIUM_AND_ABOVE");
    }

    #[test]
    fn tools_and_tool_config_serialize() {
        let request = GenerateContentRequest {
            contents: vec![],
            system_instruction: None,
            generation_config: None,
            safety_settings: None,
            tools: Some(vec![serde_json::json!({
                "functionDeclarations": [{
                    "name": "shell",
                    "description": "Run a shell command",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "command": {"type": "array", "items": {"type": "string"}}
                        },
                        "required": ["command"]
                    }
                }]
            })]),
            tool_config: Some(serde_json::json!({
                "functionCallingConfig": {"mode": "ANY"}
            })),
            thinking_config: None,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["tools"][0]["functionDeclarations"][0]["name"], "shell");
        assert_eq!(json["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
    }

    #[test]
    fn tools_none_omitted_from_serialization() {
        let request = GenerateContentRequest {
            contents: vec![],
            system_instruction: None,
            generation_config: None,
            safety_settings: None,
            tools: None,
            tool_config: None,
            thinking_config: None,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert!(json.get("tools").is_none());
        assert!(json.get("toolConfig").is_none());
    }
}

//! HTTP endpoint client for Anthropic `/messages`.

use crate::auth::SharedAuthProvider;
use crate::common::ResponseStream;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::sse::spawn_messages_stream;
use codex_client::HttpTransport;
use codex_client::RequestTelemetry;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use serde::Serialize;
use std::sync::Arc;
use tracing::instrument;

/// Metadata attached to Anthropic `/v1/messages` requests.
///
/// Currently only carries an opaque `user_id` for attribution and audit
/// logging. Anthropic forwards this value in analytics and abuse-monitoring
/// pipelines, so it should be a stable per-user identifier (OS username,
/// corporate SSO id, etc.) rather than per-session or per-request.
#[derive(Debug, Clone, Serialize)]
pub struct MessagesApiMetadata {
    /// Opaque external user identifier forwarded to Anthropic for attribution.
    pub user_id: String,
}

/// Request body for Anthropic `/v1/messages`.
#[derive(Debug, Clone, Serialize)]
pub struct MessagesApiRequest {
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    pub max_tokens: u32,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<serde_json::Value>,
    /// Anthropic `output_config` -- controls reasoning effort.
    /// Set to `{"effort": "low"|"medium"|"high"|"max"}` based on
    /// `model_reasoning_effort`. Both Anthropic-direct and Copilot CAPI
    /// paths populate this; Copilot may override via its own mapping.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MessagesApiMetadata>,
}

pub struct MessagesClient<T: HttpTransport> {
    session: EndpointSession<T>,
}

impl<T: HttpTransport> MessagesClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
        }
    }

    pub fn with_telemetry(self, request: Option<Arc<dyn RequestTelemetry>>) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
        }
    }

    #[instrument(
        name = "messages.stream_request",
        level = "info",
        skip_all,
        fields(
            transport = "messages_http",
            http.method = "POST",
            api.path = "messages"
        )
    )]
    pub async fn stream_request(
        &self,
        request: MessagesApiRequest,
        extra_headers: HeaderMap,
    ) -> Result<ResponseStream, ApiError> {
        let body = serde_json::to_value(&request)
            .map_err(|e| ApiError::Stream(format!("failed to encode messages request: {e}")))?;

        let mut headers = extra_headers;
        headers.insert(
            http::header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );

        headers.insert(
            http::HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );

        // Build anthropic-beta header by merging any betas already set in
        // extra_headers (e.g. extended-cache-ttl-2025-04-11 from the provider
        // layer) with the betas we always need here. We extract the existing
        // value first and split it so we can de-duplicate, then re-insert a
        // single merged comma-separated value. Using insert() without extract
        // would silently drop the provider-injected betas.
        let existing_betas: Vec<String> = headers
            .remove(http::HeaderName::from_static("anthropic-beta"))
            .and_then(|v| v.to_str().ok().map(|s| s.to_owned()))
            .into_iter()
            .flat_map(|s| s.split(',').map(str::trim).map(str::to_owned).collect::<Vec<_>>())
            .collect();

        let mut beta_features: Vec<&str> = Vec::new();

        // Always opt into prompt caching on the /messages wire so the upstream
        // Anthropic SSE emits cache_read_input_tokens / cache_creation_input_tokens
        // and honors `cache_control: ephemeral` breakpoints on system blocks,
        // tool definitions, and message content blocks. Prompt caching is GA on
        // the direct Anthropic API (the beta header is a no-op there) but the
        // header is still required by some proxy paths (CAPI, older LiteLLM
        // builds) to surface cache hit/miss telemetry. Sending it unconditionally
        // is safe — Anthropic ignores unknown/legacy beta tokens.
        beta_features.push("prompt-caching-2024-07-31");

        if request.thinking.is_some() {
            beta_features.push("interleaved-thinking-2025-05-14");
        }
        // Future: add effort beta when effort param is wired
        // if request.effort.is_some() {
        //     beta_features.push("effort-2025-11-24");
        // }

        // Merge provider-injected betas (e.g. extended-cache-ttl-2025-04-11)
        // without duplicating entries already in beta_features.
        let merged: Vec<String> = beta_features
            .iter()
            .map(|s| s.to_string())
            .chain(
                existing_betas
                    .into_iter()
                    .filter(|b| !beta_features.contains(&b.as_str())),
            )
            .collect();

        if !merged.is_empty() {
            let beta_value = merged.join(",").parse().map_err(|e| {
                ApiError::Stream(format!("failed to build anthropic-beta header: {e}"))
            })?;
            headers.insert(http::HeaderName::from_static("anthropic-beta"), beta_value);
        }

        let stream_response = self
            .session
            .stream_with(Method::POST, Self::path(), headers, Some(body), |_| {})
            .await?;

        Ok(spawn_messages_stream(
            stream_response.bytes,
            self.session.provider().stream_idle_timeout,
        ))
    }

    /// Relative path appended to the provider's base URL.
    ///
    /// The `Provider` joins `base_url + "/" + path`. For Anthropic direct API
    /// the base URL should be `https://api.anthropic.com/v1`; for LiteLLM
    /// proxies it's typically `https://proxy-host` (the proxy routes
    /// `POST /messages` internally).
    fn path() -> &'static str {
        "messages"
    }
}

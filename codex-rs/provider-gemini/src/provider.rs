//! `GeminiModelProvider` — the leaf provider for the Gemini
//! `streamGenerateContent` wire.
//!
//! Overrides `ModelProvider::stream` to build a `GenerateContentRequest`,
//! POST it to the Gemini SSE endpoint via `GenerateContentClient` from
//! codex-api, and bridge the `codex_api::ResponseStream` into the
//! `ProviderResponseStream` expected by the trait.
//!
//! Phase 1: provider-internal transport (no backend delegation).
//! The Gemini wire uses a different request type from Messages, so the
//! `MessagesBackend` trait does not apply. When a `GenerateContentBackend`
//! trait is justified (Phase 3: Vertex AI auth with 401 retry), we will
//! add one and shift the HTTP into core. Until then, the provider owns
//! the HTTP call directly — same pattern as the diagram's
//! "provider-internal transport" path.

use std::sync::Arc;

use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider::ModelProvider;
use codex_model_provider::ProviderAccountResult;
use codex_model_provider::ProviderAccountState;
use codex_model_provider::ProviderCapabilities;
use codex_model_provider::ProviderResponseStream;
use codex_model_provider::ProviderStreamRequest;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::WireApi;
use codex_models_manager::manager::SharedModelsManager;
use codex_models_manager::manager::StaticModelsManager;
use codex_protocol::error::Result;
use codex_protocol::openai_models::ModelsResponse;
use std::path::PathBuf;
use std::time::Duration;

use crate::model::default_gemini_safety_settings;
use crate::model::gemini_max_output_tokens;
use crate::model::supports_grounding;
use crate::request::conversation_to_gemini_contents;
use crate::request::extract_system_instruction;
use crate::request::tool_choice_to_gemini;
use crate::request::tools_to_gemini_format;
use codex_api::restore_original_tool_names_in_item;
use codex_api::tool_name_mapping;
use codex_tools::gemini_wire_tool_names;

/// Runtime provider for the Gemini `streamGenerateContent` wire.
#[derive(Clone, Debug)]
pub struct GeminiModelProvider {
    info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
}

impl GeminiModelProvider {
    #[must_use]
    pub fn new(info: ModelProviderInfo, auth_manager: Option<Arc<AuthManager>>) -> Self {
        Self { info, auth_manager }
    }
}

#[async_trait::async_trait]
impl ModelProvider for GeminiModelProvider {
    fn info(&self) -> &ModelProviderInfo {
        &self.info
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.auth_manager.clone()
    }

    async fn auth(&self) -> Option<CodexAuth> {
        match self.auth_manager.as_ref() {
            Some(am) => am.auth().await,
            None => None,
        }
    }

    fn account_state(&self) -> ProviderAccountResult {
        Ok(ProviderAccountState {
            account: None,
            requires_openai_auth: false,
        })
    }

    /// Declares provider capabilities for the Gemini wire.
    ///
    /// `web_search` is reported as `true` here so the orchestrator enables
    /// the web_search session capability when the user requests `--search`.
    /// The actual gating on per-model slug (i.e. only emitting `googleSearch`
    /// for 2.5+ models) happens inside `stream()` via `supports_grounding()`.
    ///
    /// Note: this capability is a provider-level upper bound. Individual
    /// model slugs that do not support grounding will silently omit the
    /// `googleSearch` tool on their requests (see `stream()`).
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            namespace_tools: true,
            image_generation: false,
            web_search: true,
        }
    }

    fn models_manager(
        &self,
        _codex_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        Arc::new(StaticModelsManager::new(
            self.auth_manager.clone(),
            config_model_catalog.unwrap_or_default(),
        ))
    }

    fn effective_wire_api(&self, _model_slug: &str) -> WireApi {
        WireApi::GenerateContent
    }

    async fn stream<'a>(
        &'a self,
        req: ProviderStreamRequest<'a>,
    ) -> Result<ProviderResponseStream> {
        let input = req.prompt.get_formatted_input();
        let contents = conversation_to_gemini_contents(&input);
        let system_instruction =
            extract_system_instruction(&req.prompt.base_instructions.text, &input);

        let generation_config = Some(codex_api::GenerationConfig {
            temperature: req.temperature,
            top_p: req.top_p,
            top_k: None,
            max_output_tokens: Some(gemini_max_output_tokens(&req.model_info.slug)),
            stop_sequences: None,
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
        });

        let thinking_config =
            crate::request::thinking_config_for_model(&req.model_info.slug, req.effort.as_ref());

        // Gate googleSearch on both: (a) the session has web_search capability
        // enabled, and (b) the concrete model slug supports grounding. This
        // prevents sending googleSearch to pre-2.5 models which reject it.
        // The capability flag flows from the orchestrator (message_processor.rs)
        // reading `GeminiModelProvider::capabilities().web_search`.
        let web_search_enabled = req.web_search_enabled && supports_grounding(&req.model_info.slug);

        if req.web_search_enabled && !supports_grounding(&req.model_info.slug) {
            tracing::warn!(
                slug = %req.model_info.slug,
                "web_search requested but model does not support googleSearch grounding; \
                 ignoring for this request"
            );
        }

        let tool_name_mapping = tool_name_mapping(
            gemini_wire_tool_names(&req.prompt.tools)
                .iter()
                .map(String::as_str),
        );

        let request = codex_api::GenerateContentRequest {
            contents,
            system_instruction,
            generation_config,
            safety_settings: Some(default_gemini_safety_settings()),
            tools: tools_to_gemini_format(&req.prompt.tools, web_search_enabled),
            tool_config: tool_choice_to_gemini(req.tool_choice),
            thinking_config,
        };

        // Build Provider + auth from ModelProviderInfo — same setup as
        // core's current_client_setup but self-contained in the provider.
        let mut api_provider = self
            .info
            .to_api_provider(/*auth_mode*/ None)
            .map_err(|e| codex_protocol::error::CodexErr::Stream(format!("{e}"), None))?;

        // Set User-Agent for proxy telemetry. Some proxy configurations
        // validate the UA prefix to route requests correctly.
        api_provider.headers.insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_static(concat!("XLI/", env!("CARGO_PKG_VERSION"))),
        );

        // Resolve auth: prefer AuthManager's API key, then env_key from
        // ModelProviderInfo, then no-op. If the AuthManager has no API
        // key (e.g. ChatGPT/AgentIdentity auth modes), we fall back
        // to the provider's configured env_key rather than sending an
        // empty Bearer token.
        let api_auth: codex_api::SharedAuthProvider = {
            // Try AuthManager first.
            let am_key = match &self.auth_manager {
                Some(am) => am
                    .auth()
                    .await
                    .and_then(|a| a.api_key().map(str::to_string)),
                None => None,
            };
            // Fall back to env_key from ModelProviderInfo.
            let key = am_key.or_else(|| self.info.api_key().ok().flatten());
            match key {
                Some(k) => Arc::new(codex_model_provider::BearerAuthProvider::new(k)),
                None => Arc::new(NoopAuth),
            }
        };

        let model_slug = req.model_info.slug.clone();
        // Max 3 attempts (1 initial + 2 retries). The transport layer
        // handles HTTP-level retry via RetryConfig; this outer loop
        // catches failures at the stream_request boundary.
        let max_attempts = self.info.stream_max_retries().min(3) as u32;
        let mut last_err = None;
        for attempt in 0..max_attempts {
            let transport = codex_api::ReqwestTransport::new(reqwest::Client::new());
            let client = codex_api::GenerateContentClient::new(
                transport,
                api_provider.clone(),
                api_auth.clone(),
                model_slug.clone(),
            );

            match client
                .stream_request(request.clone(), http::HeaderMap::new())
                .await
            {
                Ok(api_stream) => {
                    let codex_api::ResponseStream {
                        mut rx_event,
                        upstream_request_id,
                    } = api_stream;
                    if let Some(ref rid) = upstream_request_id {
                        tracing::Span::current().record("upstream_request_id", rid.as_str());
                        tracing::debug!(upstream_request_id = %rid, "gemini stream started");
                    }

                    let (tx, rx) = tokio::sync::mpsc::channel::<
                        codex_protocol::error::Result<codex_api::ResponseEvent>,
                    >(1600);
                    let mapping = tool_name_mapping.clone();
                    tokio::spawn(async move {
                        while let Some(event) = rx_event.recv().await {
                            let mapped = event
                                .map_err(|e| {
                                    codex_protocol::error::CodexErr::Stream(format!("{e}"), None)
                                })
                                .map(|event| match event {
                                    codex_api::ResponseEvent::OutputItemDone(mut item) => {
                                        restore_original_tool_names_in_item(&mut item, &mapping);
                                        codex_api::ResponseEvent::OutputItemDone(item)
                                    }
                                    codex_api::ResponseEvent::OutputItemAdded(mut item) => {
                                        restore_original_tool_names_in_item(&mut item, &mapping);
                                        codex_api::ResponseEvent::OutputItemAdded(item)
                                    }
                                    other => other,
                                });
                            if tx.send(mapped).await.is_err() {
                                return;
                            }
                        }
                    });

                    return Ok(ProviderResponseStream::new(rx));
                }
                Err(e) if attempt + 1 < max_attempts && is_retryable_api_error(&e) => {
                    // Honor server-supplied Retry-After delay when available,
                    // otherwise fall back to exponential backoff.
                    let delay = match &e {
                        codex_api::ApiError::Retryable { delay: Some(d), .. } => *d,
                        _ => backoff_delay(attempt),
                    };
                    tracing::warn!(
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        error = %e,
                        "gemini stream request failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    last_err = Some(e);
                }
                Err(e) => {
                    return Err(codex_api::map_api_error(e));
                }
            }
        }

        match last_err {
            Some(e) => Err(codex_api::map_api_error(e)),
            None => Err(codex_protocol::error::CodexErr::Stream(
                "gemini retry loop exhausted without recording an error".to_string(),
                None,
            )),
        }
    }
}

/// No-op auth provider for when auth is handled via provider headers.
#[derive(Debug)]
struct NoopAuth;

impl codex_api::AuthProvider for NoopAuth {
    fn add_auth_headers(&self, _headers: &mut http::HeaderMap) {}
}

/// Returns true for API errors that are worth retrying (transient
/// server issues, rate limits, connection problems).
fn is_retryable_api_error(err: &codex_api::ApiError) -> bool {
    use codex_api::ApiError;
    matches!(
        err,
        ApiError::Stream(_)
            | ApiError::Retryable { .. }
            | ApiError::ServerOverloaded
            | ApiError::Transport(_)
    )
}

/// Exponential backoff: 500ms, 1s, 2s, capped at 30s.
fn backoff_delay(attempt: u32) -> Duration {
    let base = Duration::from_millis(500);
    let max = Duration::from_secs(30);
    (base * 2u32.pow(attempt)).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_increases_exponentially() {
        assert_eq!(backoff_delay(0), Duration::from_millis(500));
        assert_eq!(backoff_delay(1), Duration::from_millis(1000));
        assert_eq!(backoff_delay(2), Duration::from_millis(2000));
        assert_eq!(backoff_delay(3), Duration::from_millis(4000));
    }

    #[test]
    fn backoff_caps_at_30_seconds() {
        assert_eq!(backoff_delay(10), Duration::from_secs(30));
        assert_eq!(backoff_delay(20), Duration::from_secs(30));
    }

    #[test]
    fn retryable_errors_classified_correctly() {
        assert!(is_retryable_api_error(&codex_api::ApiError::Stream(
            "connection reset".into()
        )));
        assert!(is_retryable_api_error(&codex_api::ApiError::Retryable {
            message: "rate limited".into(),
            delay: None,
        }));
        assert!(is_retryable_api_error(
            &codex_api::ApiError::ServerOverloaded
        ));
        // Non-retryable:
        assert!(!is_retryable_api_error(
            &codex_api::ApiError::ContextWindowExceeded
        ));
        assert!(!is_retryable_api_error(
            &codex_api::ApiError::InvalidRequest {
                message: "bad request".into()
            }
        ));
    }
}

//! Messages-wire dispatch extracted from `client.rs` (Sortie 1 §5b).
//!
//! This module owns the entry point (`stream_via_provider`), the transport
//! retry loop (`run_messages_turn`), the provider-aware 401 handler, and the
//! `MessagesBackendAdapter` bridge that connects `codex-model-provider`'s
//! `MessagesBackend` trait to `ModelClientSession`'s transport layer.
//!
//! Keeping this code in its own file means `client.rs` stays structurally
//! close to upstream — the only Messages/Copilot surface left there is
//! a three-line match arm that delegates here.

use codex_api::ApiError;
use codex_api::MessagesApiRequest;
use codex_api::MessagesClient as ApiMessagesClient;
use codex_api::ReqwestTransport;
use codex_api::TransportError;
use codex_api::map_api_error;
use codex_login::AuthManager;
use codex_login::default_client::build_reqwest_client;
use codex_model_provider::MessagesBackend;
use codex_model_provider::ProviderStreamRequest;
use codex_otel::SessionTelemetry;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_response_debug_context::extract_response_debug_context;
use codex_rollout_trace::InferenceTraceContext;
use http::HeaderMap as ApiHeaderMap;
use reqwest::StatusCode;
use tracing::instrument;

use crate::client::AuthRequestTelemetryContext;
use crate::client::ModelClient;
use crate::client::ModelClientSession;
use crate::client::PendingUnauthorizedRetry;
use crate::client::RequestRouteTelemetry;
use crate::client::UnauthorizedRecoveryExecution;
use crate::client::handle_unauthorized;
use crate::client::map_response_stream;
use crate::client_common::Prompt;
use crate::client_common::ResponseStream;
use crate::config::CacheRetention;
use crate::config::CacheRetentionByBlock;
use crate::config::ModelEffort;
use crate::config::SamplingParams;
use codex_model_provider::CacheRetentionByBlockSetting;
use codex_model_provider::CacheRetentionSetting;

// ---------------------------------------------------------------------------
// ModelClientSession — Messages-wire entry point + transport loop
// ---------------------------------------------------------------------------

impl ModelClientSession {
    /// Streams a turn via the Anthropic Messages API (`/v1/messages`).
    #[allow(clippy::too_many_arguments)]
    #[instrument(
        name = "model_client.stream_via_provider",
        level = "info",
        skip_all,
        fields(
            model = %model_info.slug,
            wire_api = "messages",
            transport = "messages_http",
            http.method = "POST",
            api.path = "messages",
        )
    )]
    pub(crate) async fn stream_via_provider(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        service_tier: Option<String>,
        sampling: SamplingParams,
        turn_metadata_header: Option<&str>,
        inference_trace: &InferenceTraceContext,
        web_search_mode: Option<WebSearchMode>,
        cache_retention: Option<CacheRetention>,
        cache_retention_by_block: CacheRetentionByBlock,
        model_effort: Option<ModelEffort>,
    ) -> Result<ResponseStream> {
        // Sortie 1: dispatch through the `ModelProvider` trait. The
        // provider crate owns wire-specific request construction; the
        // backend adapter runs the 401 retry loop + transport dispatch.
        // ensure_session_ctx lets providers do lazy per-session init
        // (e.g. Copilot CAPI context mint).
        self.client.state.provider.ensure_session_ctx().await?;
        let tool_choice = self.client.state.tool_choice.clone();
        // Upstream now threads service tier as a request string; map it back to
        // the provider-level `ServiceTier` enum for non-Responses wires.
        let service_tier = service_tier
            .as_deref()
            .and_then(ServiceTier::from_request_value);
        let messages_metadata_user_id = self.client.state.messages_metadata_user_id.clone();
        let backend = MessagesBackendAdapter {
            session: self,
            session_telemetry,
            inference_trace,
        };
        // Source `web_search_enabled` from the per-turn `WebSearchMode` first,
        // then fall back to the provider's static capability upper bound.
        // `WebSearchMode::Disabled` suppresses googleSearch even when the
        // provider capability says true. `Live` / `Cached` / `None` (unset)
        // fall through to the capability flag so providers without web search
        // (Anthropic, Copilot) are unaffected. Per-slug gating (gemini-2.5+)
        // is enforced inside `GeminiModelProvider::stream()`.
        let web_search_enabled = match web_search_mode {
            Some(WebSearchMode::Disabled) => false,
            // Live, Cached, and unset all defer to the provider capability flag.
            // Note: if a future WebSearchMode::Enabled variant is added, it will
            // fall here and return the capability bool — an explicit Enabled cannot
            // override a provider that declares web_search: false. This is intentional;
            // the capability is a hard ceiling, not a suggestion.
            _ => self.client.state.provider.capabilities().web_search,
        };
        // Convert codex_config::CacheRetention to the leaf-safe mirror type.
        let retention_setting = match cache_retention {
            Some(CacheRetention::OneHour) => CacheRetentionSetting::OneHour,
            _ => CacheRetentionSetting::Ephemeral,
        };
        let by_block_setting = CacheRetentionByBlockSetting {
            system: cache_retention_by_block.system.map(|r| match r {
                CacheRetention::OneHour => CacheRetentionSetting::OneHour,
                CacheRetention::Ephemeral => CacheRetentionSetting::Ephemeral,
            }),
            tool: cache_retention_by_block.tool.map(|r| match r {
                CacheRetention::OneHour => CacheRetentionSetting::OneHour,
                CacheRetention::Ephemeral => CacheRetentionSetting::Ephemeral,
            }),
            user_last: cache_retention_by_block.user_last.map(|r| match r {
                CacheRetention::OneHour => CacheRetentionSetting::OneHour,
                CacheRetention::Ephemeral => CacheRetentionSetting::Ephemeral,
            }),
            user_previous: cache_retention_by_block.user_previous.map(|r| match r {
                CacheRetention::OneHour => CacheRetentionSetting::OneHour,
                CacheRetention::Ephemeral => CacheRetentionSetting::Ephemeral,
            }),
        };
        // Pre-stringify model_effort for the provider (avoids codex-config dep in provider crates).
        let model_effort_str: Option<String> = model_effort.map(|e| match e {
            ModelEffort::Low => "low".to_string(),
            ModelEffort::Medium => "medium".to_string(),
            ModelEffort::High => "high".to_string(),
            ModelEffort::Max => "max".to_string(),
        });
        let req = ProviderStreamRequest::builder(prompt, model_info, &backend)
            .with_effort(effort)
            .with_summary(summary)
            .with_service_tier(service_tier)
            .with_sampling(sampling.temperature, sampling.top_p, sampling.top_k)
            .with_turn_metadata_header(turn_metadata_header)
            .with_tool_choice(tool_choice.as_ref())
            .with_messages_metadata_user_id(messages_metadata_user_id.as_deref())
            .with_web_search_enabled(web_search_enabled)
            .with_cache_retention(retention_setting)
            .with_cache_retention_by_block(by_block_setting)
            .with_model_effort_default(model_effort_str.as_deref())
            .build();
        self.client.state.provider.stream(req).await
    }

    /// Runs one Messages-wire turn against the transport layer.
    ///
    /// Owns the 401 retry loop and the per-attempt
    /// `current_client_setup` refresh. Provider-specific auth setup
    /// (base-URL, headers, bearer minting) is handled by the
    /// provider's `api_provider()` / `api_auth()` overrides.
    ///
    /// Called from [`MessagesBackendAdapter::execute_messages_turn`]
    /// after the provider crate has built a wire-typed
    /// `MessagesApiRequest`. All wire-specific construction logic
    /// lives in the provider crates; this method is purely transport
    /// orchestration.
    pub(crate) async fn run_messages_turn(
        &self,
        request: MessagesApiRequest,
        extra_headers: ApiHeaderMap,
        session_telemetry: &SessionTelemetry,
        inference_trace: &InferenceTraceContext,
    ) -> Result<ResponseStream> {
        let auth_manager = self.client.state.provider.auth_manager();
        let mut auth_recovery = auth_manager
            .as_ref()
            .map(AuthManager::unauthorized_recovery);
        let mut pending_retry = PendingUnauthorizedRetry::default();
        loop {
            let mut client_setup = self.client.current_client_setup().await?;
            // Let the provider transform the base URL for Messages wire
            // (e.g. Copilot splices /v1 for the Enterprise CAPI endpoint).
            self.client
                .state
                .provider
                .transform_messages_base_url(&mut client_setup.api_provider.base_url);

            let transport = ReqwestTransport::new(build_reqwest_client());
            let request_auth_context = AuthRequestTelemetryContext::new(
                client_setup
                    .auth
                    .as_ref()
                    .map(codex_login::CodexAuth::auth_mode),
                client_setup.api_auth.as_ref(),
                pending_retry,
            );
            let (request_telemetry, _sse_telemetry) = Self::build_streaming_telemetry(
                session_telemetry,
                request_auth_context,
                RequestRouteTelemetry::for_endpoint("/messages"),
                self.client.state.auth_env_telemetry.clone(),
            );
            let inference_trace_attempt = inference_trace.start_attempt();
            let client =
                ApiMessagesClient::new(transport, client_setup.api_provider, client_setup.api_auth)
                    .with_telemetry(Some(request_telemetry));

            match client
                .stream_request(request.clone(), extra_headers.clone())
                .await
            {
                Ok(stream) => {
                    let (stream, _) = map_response_stream(
                        stream,
                        session_telemetry.clone(),
                        inference_trace_attempt,
                        Default::default(),
                    );
                    self.client.state.provider.note_request_succeeded();
                    return Ok(stream);
                }
                Err(ApiError::Transport(
                    unauthorized_transport @ TransportError::Http { status, .. },
                )) if status == StatusCode::UNAUTHORIZED => {
                    inference_trace_attempt.record_failed(&unauthorized_transport, None, &[]);
                    pending_retry = PendingUnauthorizedRetry::from_recovery(
                        handle_unauthorized_for_provider(
                            &self.client,
                            unauthorized_transport,
                            &mut auth_recovery,
                            session_telemetry,
                        )
                        .await?,
                    );
                    continue;
                }
                Err(err) => {
                    let err = map_api_error(err);
                    inference_trace_attempt.record_failed(&err, None, &[]);
                    return Err(err);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Provider-aware 401 handler
// ---------------------------------------------------------------------------

/// Provider-aware wrapper around [`handle_unauthorized`].
///
/// Gives the provider a chance to handle the 401 via its own auth
/// recovery path (e.g. Copilot's CAPI bearer force-refresh). If the
/// provider handles it, the caller retries. Otherwise, falls through
/// to the standard `CodexAuth` recovery ladder.
pub(crate) async fn handle_unauthorized_for_provider(
    client: &ModelClient,
    transport: TransportError,
    auth_recovery: &mut Option<codex_login::UnauthorizedRecovery>,
    session_telemetry: &SessionTelemetry,
) -> Result<UnauthorizedRecoveryExecution> {
    // Let the provider attempt its own recovery first.
    match client.state.provider.try_refresh_auth().await {
        Ok(true) => {
            return Ok(UnauthorizedRecoveryExecution {
                mode: "provider",
                phase: "refresh_auth",
            });
        }
        Ok(false) => {
            // Provider has no recovery path; fall through.
        }
        Err(CodexErr::Fatal(provider_msg)) => {
            // Enrich with transport diagnostic context (F6) so
            // operators can locate the failed request in proxy logs.
            let debug = extract_response_debug_context(&transport);
            let request_id = debug.request_id.as_deref().unwrap_or("<no x-request-id>");
            let cf_ray = debug.cf_ray.as_deref().unwrap_or("<no cf-ray>");
            let auth_err = debug.auth_error.as_deref().unwrap_or("<no body error>");
            return Err(CodexErr::Fatal(format!(
                "{provider_msg} (wire={wire:?}, base_url={base}, \
                 request_id={request_id}, cf_ray={cf_ray}, \
                 upstream_error={auth_err})",
                wire = client.state.provider.info().wire_api,
                base = client
                    .state
                    .provider
                    .info()
                    .base_url
                    .as_deref()
                    .unwrap_or("<no base_url>"),
            )));
        }
        Err(e) => return Err(e),
    }
    handle_unauthorized(transport, auth_recovery, session_telemetry).await
}

// ---------------------------------------------------------------------------
// MessagesBackendAdapter — bridge from provider trait to core transport
// ---------------------------------------------------------------------------

/// Adapter that implements [`codex_model_provider::MessagesBackend`]
/// by forwarding to [`ModelClientSession::run_messages_turn`].
///
/// Constructed per-turn in [`ModelClientSession::stream_via_provider`]
/// so it can borrow the session and the per-turn telemetry / trace
/// context for the duration of the provider's `stream` call. All
/// fields are `&` references; the adapter is intentionally short-lived
/// and not `Clone`.
struct MessagesBackendAdapter<'a> {
    session: &'a ModelClientSession,
    session_telemetry: &'a SessionTelemetry,
    inference_trace: &'a InferenceTraceContext,
}

#[async_trait::async_trait]
impl MessagesBackend for MessagesBackendAdapter<'_> {
    async fn execute_messages_turn(
        &self,
        request: MessagesApiRequest,
        extra_headers: ApiHeaderMap,
    ) -> Result<ResponseStream> {
        self.session
            .run_messages_turn(
                request,
                extra_headers,
                self.session_telemetry,
                self.inference_trace,
            )
            .await
    }
}

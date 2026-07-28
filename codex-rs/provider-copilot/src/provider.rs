//! `CopilotModelProvider` — runtime provider for the GitHub Copilot
//! LLM plane.
//!
//! Owns ALL Copilot-specific dispatch, extracted from
//! `core/src/client.rs` by Sortie 1:
//!
//!   - `api_provider()` / `api_auth()`: CAPI bearer minting, base-URL
//!     override with `/v1` prefix, idle-timeout, shared headers.
//!   - `stream()`: Messages wire (with anthropic-beta prompt-caching
//!     header) and chat-completions adapter (legacy slugs).
//!   - `try_refresh_auth()` / `note_request_succeeded()`: 401 retry
//!     budget with session-scoped force-refresh counter.
//!   - `ensure_session_ctx()`: lazy CopilotCtx initialization with
//!     first-turn TTY/TOS guards.
//!   - `effective_wire_api()`: slug-based route table via
//!     `codex_copilot_adapter::route_for_model`.
//!   - `extra_request_headers()`: the four shared Copilot transport
//!     headers.
//!   - `supports_websockets()`: returns `false` (enterprise proxy is
//!     HTTP-SSE only).

use std::path::PathBuf;
use std::sync::Arc;

use codex_copilot_adapter::CopilotWire;
use codex_copilot_adapter::route_for_model;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider::ModelProvider;
use codex_model_provider::ProviderAccountResult;
use codex_model_provider::ProviderAccountState;
use codex_model_provider::ProviderResponseStream;
use codex_model_provider::ProviderStreamRequest;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::WireApi;
use codex_models_manager::manager::SharedModelsManager;
use codex_models_manager::manager::StaticModelsManager;
use codex_models_manager::ProviderCaps;
use codex_models_manager::ProviderCapsPatch;
use codex_protocol::openai_models::ModelsResponse;
use codex_provider_anthropic::Sampling;
use codex_provider_anthropic::build_messages_extra_headers_with_retention;
use codex_provider_anthropic::build_messages_request;
use codex_model_provider::CacheRetentionByBlockSetting;
use codex_model_provider::CacheRetentionSetting;
use http::HeaderMap;

use crate::headers::stamp_copilot_shared_headers;
use crate::url::ensure_v1_prefix;

use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

/// Maximum number of Copilot CAPI bearer force-refreshes per session.
///
/// Cap of 3 lets a single transient 401 + refresh succeed, plus headroom
/// for one stale-bearer turn — but blocks a wedged seat from spinning
/// `/copilot_internal/v2/token` indefinitely.
pub(crate) const COPILOT_MAX_FORCE_REFRESHES_PER_SESSION: u32 = 3;

/// Runtime provider for the GitHub Copilot LLM plane.
///
/// Constructed with a pre-resolved `ModelProviderInfo` whose
/// `wire_api == WireApi::Copilot` and an optional `AuthManager`.
#[derive(Clone, Debug)]
pub struct CopilotModelProvider {
    info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
    /// Session-scoped CAPI auth + `/models` cache slot.
    ///
    /// Set lazily by [`Self::ensure_ctx`] on the first Copilot turn.
    /// `core::client::ensure_copilot_ctx` mints the session ctx on
    /// the first Copilot turn. Until set, [`Self::populate_model_info`]
    /// is a no-op and pre-flight effort validation is skipped — the
    /// provider falls back to family-default heuristics, matching
    /// pre-engagement-2 behavior.
    ///
    /// `Arc<OnceLock<...>>` rather than `Option<Arc<...>>` because the
    /// provider lives behind `Arc<dyn ModelProvider>` and gets cloned
    /// liberally; interior mutability lets a single `attach_ctx` call
    /// reach every clone.
    ctx: Arc<tokio::sync::OnceCell<Arc<codex_copilot_adapter::CopilotCtx>>>,
    /// Session-scoped CAPI bearer force-refresh counter. Incremented every
    /// time [`Self::try_refresh_auth`] mints a fresh bearer in response to
    /// a 401. Capped at [`COPILOT_MAX_FORCE_REFRESHES_PER_SESSION`] so a
    /// misbehaving CAPI endpoint cannot spin indefinitely.
    force_refresh_attempts: Arc<AtomicU32>,
    /// Test-only injection seam for the CAPI catalog. Production code
    /// goes through `ctx.models()`; tests bypass the ctx entirely.
    #[cfg(test)]
    test_catalog: Option<codex_copilot_adapter::CapiModelCatalog>,
}

impl CopilotModelProvider {
    /// Constructs a `CopilotModelProvider` from pre-resolved inputs.
    #[must_use]
    pub fn new(info: ModelProviderInfo, auth_manager: Option<Arc<AuthManager>>) -> Self {
        Self {
            info,
            auth_manager,
            ctx: Arc::new(tokio::sync::OnceCell::new()),
            force_refresh_attempts: Arc::new(AtomicU32::new(0)),
            #[cfg(test)]
            test_catalog: None,
        }
    }

    /// Externally attach a pre-built [`CopilotCtx`].
    ///
    /// Idempotent: subsequent calls are silently ignored
    /// (`OnceCell::set` semantics) so concurrent first-turn paths
    /// can't fight each other.
    ///
    /// In normal operation the provider initializes its own ctx via
    /// [`ensure_ctx`](Self::ensure_ctx) (called from
    /// `ensure_session_ctx`). This method exists for test injection
    /// and any future orchestration path that pre-mints a ctx
    /// externally.
    pub(crate) fn attach_ctx(&self, ctx: Arc<codex_copilot_adapter::CopilotCtx>) {
        // OnceCell::set returns Err if already initialized; we ignore
        // because by design this is at-most-once per session and the
        // first writer wins.
        let _ = self.ctx.set(ctx);
    }

    /// Returns the attached ctx, if any. For internal use by the
    /// `populate_model_info` and pre-flight paths.
    fn ctx(&self) -> Option<&Arc<codex_copilot_adapter::CopilotCtx>> {
        self.ctx.get()
    }
    /// Test-only: pre-populate the CAPI catalog so `populate_model_info`
    /// and the pre-flight check can run without minting a real
    /// `CopilotCtx`.
    #[cfg(test)]
    pub(crate) fn with_test_catalog(
        mut self,
        catalog: codex_copilot_adapter::CapiModelCatalog,
    ) -> Self {
        self.test_catalog = Some(catalog);
        self
    }

    /// Lazily initializes the session-scoped `CopilotCtx`.
    ///
    /// Uses `tokio::sync::OnceCell::get_or_try_init` so concurrent
    /// first-turn callers converge on a single shared context without
    /// duplicate CAPI mints or redundant TTY/TOS side effects.
    async fn ensure_ctx(
        &self,
    ) -> codex_protocol::error::Result<Arc<codex_copilot_adapter::CopilotCtx>> {
        let ctx = self
            .ctx
            .get_or_try_init(|| async {
                codex_copilot_adapter::enforce_copilot_first_turn_guards()
                    .map_err(|e| codex_protocol::error::CodexErr::Fatal(format!("copilot: {e}")))?;
                let http = codex_copilot_adapter::build_copilot_auth_http_client();
                let mut extra_headers = http::HeaderMap::new();
                stamp_copilot_shared_headers(&mut extra_headers);
                let ctx = codex_copilot_adapter::CopilotCtx::init_with_headers(http, extra_headers)
                    .await
                    .map_err(|e| {
                        codex_protocol::error::CodexErr::Fatal(format!(
                            "copilot: failed to init auth context: {e}"
                        ))
                    })?;
                Ok::<_, codex_protocol::error::CodexErr>(Arc::new(ctx))
            })
            .await?;
        Ok(Arc::clone(ctx))
    }

    /// Apply CAPI /models metadata to a `ModelInfo` in place. Idempotent:
    /// missing slugs / fields leave the corresponding `ModelInfo` field
    /// untouched.
    /// Hardcoded CAPI per-slug limits, verified 2026-05-28 against
    /// production `/models` (api.enterprise.githubcopilot.com).
    ///
    /// CAPI sometimes returns null for `max_prompt_tokens` and
    /// `max_context_window_tokens`, causing the family-default
    /// heuristic to persist (1M for Claude 4.6+, which is wrong
    /// for Copilot-hosted models). These values are the ground
    /// truth from production CAPI `/models`.
    ///
    /// Returns `(max_prompt_tokens, max_context_window_tokens,
    /// max_output_tokens)`. Output cap matters too: family defaults
    /// give Opus 128K output, but CAPI caps Opus 4.6 / 4.7 at 32K,
    /// Opus 4.8 at 64K. Haiku-4.5 is the opposite — family default
    /// 8K, CAPI 64K, so we'd under-utilize the budget without the
    /// override.
    fn hardcoded_capi_limits(slug: &str) -> Option<(i64, i64, i64)> {
        let s = slug.to_ascii_lowercase();
        // ── Claude (Anthropic via Copilot) ──
        if s.contains("claude-opus-4.8") {
            // 4.8 raised max_output_tokens from 32K (4.6/4.7) to 64K.
            return Some((168_000, 200_000, 64_000));
        }
        if s.contains("claude-opus-4.7") {
            return Some((168_000, 200_000, 32_000));
        }
        if s.contains("claude-opus-4.6") {
            return Some((168_000, 200_000, 32_000));
        }
        if s.contains("claude-opus-4.5") {
            return Some((168_000, 200_000, 32_000));
        }
        if s.contains("claude-sonnet-4.6") || s.contains("claude-sonnet-4.5") {
            return Some((168_000, 200_000, 32_000));
        }
        if s.contains("claude-haiku-4.5") {
            // Family default gives Haiku only 8K output; CAPI offers 64K.
            return Some((136_000, 200_000, 64_000));
        }
        // ── GPT (OpenAI via Copilot) ──
        if s.starts_with("gpt-5-mini") {
            return Some((128_000, 264_000, 64_000));
        }
        if s.starts_with("gpt-5.4-mini") {
            return Some((272_000, 400_000, 128_000));
        }
        if s.starts_with("gpt-5.4") {
            return Some((272_000, 400_000, 128_000));
        }
        if s.starts_with("gpt-5.3") {
            return Some((272_000, 400_000, 128_000));
        }
        if s.starts_with("gpt-5.2") {
            return Some((272_000, 400_000, 128_000));
        }
        if s.starts_with("gpt-5.5") {
            return Some((272_000, 400_000, 128_000));
        }
        // Unknown slug — no hardcoded data, fall through.
        None
    }

    fn apply_capi_override(
        info: &mut codex_protocol::openai_models::ModelInfo,
        catalog: &codex_copilot_adapter::CapiModelCatalog,
    ) {
        // Two-tier override strategy. Both tiers MUST apply
        // unconditionally (i.e. not gated on
        // `used_fallback_model_metadata`), because for Copilot-routed
        // Claude the static family default is "Claude is a known
        // family => use the Anthropic-direct 1M-context beta SKU",
        // which is wrong for Copilot's 200K cap. The TUI rendered
        // that 1M as 950K (after the 95% effective window discount)
        // and lied about the real prompt budget for the entire first
        // turn — the bug Delta chased for weeks.
        //
        // Tier 1: hardcoded known CAPI limits (deterministic, fires
        // at session-init time before any turn has minted a CAPI
        // bearer). Verified 2026-05-28 against production CAPI
        // /models for opus-4.5/4.6/4.7/4.8, sonnet-4.5/4.6,
        // haiku-4.5, gpt-5-mini, gpt-5.2/5.3/5.4/5.4-mini/5.5.
        if let Some((prompt, total, output)) = Self::hardcoded_capi_limits(&info.slug) {
            info.context_window = Some(prompt);
            info.max_context_window = Some(total);
            ProviderCaps::merge_override(
                &info.slug,
                ProviderCapsPatch {
                    max_output_tokens: Some(Some(output)),
                    ..Default::default()
                },
            );
            info.used_fallback_model_metadata = false;
        }

        // Tier 2: live CAPI catalog wins over tier 1 when present.
        // Source of truth for slugs we haven't hardcoded yet, plus
        // a safety net if CAPI raises a limit (e.g. opus-4.8 raised
        // max_output_tokens from 32K to 64K).
        if let Some(capi_info) = catalog.get(&info.slug) {
            if let Some(prompt_cap) = capi_info.limits.max_prompt_tokens {
                info.context_window = Some(prompt_cap);
            }
            if let Some(total_cap) = capi_info.limits.max_context_window_tokens {
                info.max_context_window = Some(total_cap);
            }
            if let Some(output_cap) = capi_info.limits.max_output_tokens {
                ProviderCaps::merge_override(
                    &info.slug,
                    ProviderCapsPatch {
                        max_output_tokens: Some(Some(output_cap)),
                        ..Default::default()
                    },
                );
            }
            info.used_fallback_model_metadata = false;
        }
        tracing::debug!(
            target: "copilot::provider",
            slug = %info.slug,
            context_window = ?info.context_window,
            max_context_window = ?info.max_context_window,
            "apply_capi_override: enriched ModelInfo from CAPI /models"
        );
    }

    /// Streams a turn via the legacy Copilot chat-completions adapter.
    ///
    /// Called from [`Self::stream`] when `effective_wire_api` returns
    /// `WireApi::Copilot` (legacy pre-5 GPT slugs that the CAPI route
    /// table doesn't map to a native wire). The adapter owns its own
    /// auth, headers, and 401 retry -- this is a thin bridge that
    /// converts the adapter's event stream into a `ResponseStream`.
    async fn stream_chat_completions(
        &self,
        req: &ProviderStreamRequest<'_>,
    ) -> codex_protocol::error::Result<ProviderResponseStream> {
        use futures::StreamExt;

        let input = req.prompt.get_formatted_input();
        let model_slug = req.model_info.slug.clone();
        let tools = req.prompt.tools.clone();

        let mut adapter_stream = codex_copilot_adapter::stream(&input, &model_slug, &tools)
            .await
            .map_err(|e| codex_protocol::error::CodexErr::Fatal(format!("copilot adapter: {e}")))?;

        let (tx, rx) = tokio::sync::mpsc::channel::<
            codex_protocol::error::Result<codex_prompt::ResponseEvent>,
        >(32);
        tokio::spawn(async move {
            while let Some(item) = adapter_stream.next().await {
                let converted = item.map_err(|e| {
                    codex_protocol::error::CodexErr::Fatal(format!("copilot adapter: {e}"))
                });
                if tx.send(converted).await.is_err() {
                    return;
                }
            }
        });

        Ok(codex_prompt::ResponseStream::new(rx))
    }
}

#[async_trait::async_trait]
impl ModelProvider for CopilotModelProvider {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn info(&self) -> &ModelProviderInfo {
        &self.info
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.auth_manager.clone()
    }

    async fn auth(&self) -> Option<CodexAuth> {
        match self.auth_manager.as_ref() {
            Some(auth_manager) => auth_manager.auth().await,
            None => None,
        }
    }

    fn account_state(&self) -> ProviderAccountResult {
        Ok(ProviderAccountState {
            account: None,
            requires_openai_auth: false,
        })
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

    /// Dispatches a model slug to its Copilot sub-wire via the static
    /// route table in `codex-copilot-adapter`.
    ///
    /// The mapping:
    ///
    /// * `CopilotWire::Messages` → `WireApi::Messages` (native Claude)
    /// * `CopilotWire::Responses` → `WireApi::Responses` (native GPT-5.x)
    /// * `CopilotWire::ChatCompletions` → `WireApi::Copilot` (the legacy
    ///   chat-completions adapter path, kept for pre-5 GPT models)
    fn effective_wire_api(&self, model_slug: &str) -> WireApi {
        match route_for_model(model_slug) {
            CopilotWire::Messages => WireApi::Messages,
            CopilotWire::Responses => WireApi::Responses,
            CopilotWire::ChatCompletions => WireApi::Copilot,
        }
    }

    /// Stamps the four shared Copilot headers on every request. The
    /// implementation lives in [`crate::headers::stamp_copilot_shared_headers`].
    fn extra_request_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        stamp_copilot_shared_headers(&mut headers);
        headers
    }

    /// Splices `/v1` into the CAPI base URL for Messages-wire requests.
    ///
    /// Enterprise Copilot exposes Anthropic at `/v1/messages` while the
    /// Responses endpoint sits at `<host>/responses` without the prefix.
    /// This transform is applied by `run_messages_turn` in core after
    /// `current_client_setup` resolves the base URL from the CAPI
    /// envelope's `endpoints.api`.
    fn transform_messages_base_url(&self, url: &mut String) {
        ensure_v1_prefix(url);
    }

    /// Builds a Messages-wire request and delegates to the backend.
    ///
    /// When the Copilot route table sends a Claude slug to
    /// `WireApi::Messages`, `core/src/client.rs::stream_messages_api`
    /// calls `provider.stream(req)`. The backend
    /// (`MessagesBackendAdapter` in core) runs the turn with Copilot
    /// auth, base-url splicing, and 401 retries.
    ///
    /// The request construction reuses the same pure functions from
    /// `codex-provider-anthropic` that `AnthropicMessagesProvider`
    /// uses — the wire shape is identical; only the transport
    /// orchestration differs (Copilot auth vs. direct Anthropic key).
    /// Returns Copilot provider config with the CAPI endpoint and
    /// shared headers.
    ///
    /// # Snapshot coherence
    ///
    /// `api_provider` and `api_auth` each take a separate CAPI
    /// snapshot. This is safe because `force_refresh` only fires
    /// after a 401 terminates the current attempt; the next retry
    /// iteration calls `current_client_setup` fresh, so both
    /// methods see the same post-refresh state.
    async fn api_provider(&self) -> codex_protocol::error::Result<codex_api::Provider> {
        let ctx = self.ensure_ctx().await?;
        let snapshot = ctx.snapshot().await.map_err(|e| {
            codex_protocol::error::CodexErr::Fatal(format!(
                "copilot: failed to mint CAPI bearer: {e}"
            ))
        })?;
        let mut api_provider = self.info.to_api_provider(None)?;
        api_provider.base_url = snapshot.endpoints_api;
        // Copilot's enterprise proxy occasionally stalls mid-stream. Use
        // 30 minutes as a ceiling — TCP/TLS surfaces real disconnects
        // well before this fires.
        api_provider.stream_idle_timeout = std::time::Duration::from_secs(1800);
        stamp_copilot_shared_headers(&mut api_provider.headers);
        Ok(api_provider)
    }

    async fn api_auth(&self) -> codex_protocol::error::Result<codex_api::SharedAuthProvider> {
        let ctx = self.ensure_ctx().await?;
        let snapshot = ctx.snapshot().await.map_err(|e| {
            codex_protocol::error::CodexErr::Fatal(format!(
                "copilot: failed to mint CAPI bearer: {e}"
            ))
        })?;
        Ok(std::sync::Arc::new(
            codex_model_provider::BearerAuthProvider {
                token: Some(snapshot.into_bearer()),
                account_id: None,
                is_fedramp_account: false,
            },
        ))
    }

    async fn ensure_session_ctx(&self) -> codex_protocol::error::Result<()> {
        self.ensure_ctx().await?;
        Ok(())
    }

    async fn try_refresh_auth(&self) -> codex_protocol::error::Result<bool> {
        let prior = self.force_refresh_attempts.fetch_add(1, Ordering::AcqRel);
        if prior >= COPILOT_MAX_FORCE_REFRESHES_PER_SESSION {
            // Clamp at cap so repeat 401s after exhaustion don't grow
            // the counter unboundedly (cosmetic: error message says
            // "after N" where N = cap, not N+1, N+2, ...).
            self.force_refresh_attempts
                .store(COPILOT_MAX_FORCE_REFRESHES_PER_SESSION, Ordering::Release);
            return Err(codex_protocol::error::CodexErr::Fatal(format!(
                "copilot: 401 persists after {cap} CAPI bearer refresh attempt(s) this session; \
                 seat may be revoked, plan disabled, or endpoint rejecting tokens",
                cap = COPILOT_MAX_FORCE_REFRESHES_PER_SESSION,
            )));
        }
        let ctx = self.ensure_ctx().await?;
        ctx.force_refresh().await.map_err(|e| {
            codex_protocol::error::CodexErr::Fatal(format!(
                "copilot: 401 retry failed to mint fresh bearer: {e}"
            ))
        })?;
        Ok(true)
    }

    fn note_request_succeeded(&self) {
        self.force_refresh_attempts.store(0, Ordering::Release);
    }

    async fn stream<'a>(
        &'a self,
        req: ProviderStreamRequest<'a>,
    ) -> codex_protocol::error::Result<ProviderResponseStream> {
        // Pre-flight: validate that the user-set effort is in CAPI's
        // allow-list for this slug. Fails fast with a clear message
        // before we burn a turn against a wire that will 400 us.
        // Skipped when ctx is unattached (older construction paths).
        #[cfg(test)]
        let test_catalog = self.test_catalog.clone();
        #[cfg(test)]
        let _ctx_unused = self.ctx();
        let catalog_for_preflight: Option<codex_copilot_adapter::CapiModelCatalog> = {
            #[cfg(test)]
            {
                test_catalog
            }
            #[cfg(not(test))]
            {
                None
            }
        };
        let catalog_for_preflight = match catalog_for_preflight {
            Some(c) => Some(c),
            None => match self.ctx() {
                Some(ctx) => Some(ctx.models().await),
                None => None,
            },
        };
        if let Some(catalog) = catalog_for_preflight {
            let slug = req.model_info.slug.as_str();
            let user_effort = effort_to_capi_str(req.effort);
            if let Some(effort_str) = user_effort {
                if let Some(capi_info) = catalog.get(slug) {
                    let allowed = &capi_info.supports.reasoning_effort;
                    if !allowed.is_empty() && !allowed.iter().any(|s| s == effort_str) {
                        return Err(codex_protocol::error::CodexErr::Fatal(format!(
                            "copilot: model {slug} does not support effort='{effort_str}'. \
                             Allowed values per CAPI /models: [{allowed_list}]. \
                             Edit your config (model_reasoning_effort) or pick a profile \
                             whose model accepts this effort tier.",
                            allowed_list = allowed.join(", "),
                        )));
                    }
                }
            }
        }
        let sampling = Sampling {
            temperature: req.temperature,
            top_p: req.top_p,
            top_k: req.top_k,
        };
        let output_effort = effort_to_capi_str(req.effort);
        // Check the effective wire for this slug. Claude rides native
        // Messages; GPT-5.x rides Responses (core dispatches that
        // before reaching us); legacy slugs fall to chat-completions.
        let effective = self.effective_wire_api(&req.model_info.slug);
        if matches!(effective, WireApi::Responses) {
            // GPT-5.x routed via Copilot lands on the Responses wire;
            // core dispatches that path directly to stream_responses_api
            // and never calls provider.stream(). Guard defensively.
            return Err(codex_protocol::error::CodexErr::Fatal(format!(
                "copilot: provider.stream() called for Responses-wire slug '{}';                  this should be dispatched by core's Responses path, not the provider",
                req.model_info.slug,
            )));
        }
        if matches!(effective, WireApi::Copilot) {
            return self.stream_chat_completions(&req).await;
        }
        let request = build_messages_request(
            req.prompt,
            req.model_info,
            req.effort,
            sampling,
            req.tool_choice,
            req.messages_metadata_user_id,
            Some(output_effort),
            // Copilot path: cache retention is driven by req fields too,
            // but Copilot does not support 1h TTL — pass Ephemeral default.
            req.cache_retention,
            req.cache_retention_by_block.clone(),
            req.model_effort_default,
        );
        // Copilot CAPI: inject anthropic-beta headers.
        // build_messages_extra_headers_with_retention emits:
        //   - nothing when no 1h beta needed (messages.rs always adds prompt-caching)
        //   - "prompt-caching-2024-07-31,extended-cache-ttl-2025-04-11" when 1h active
        // The codex-api messages.rs layer merges these with its own unconditional
        // prompt-caching entry, so we do not need to insert it again here.
        let needs_1h_beta = req.cache_retention == CacheRetentionSetting::OneHour
            || req.cache_retention_by_block != CacheRetentionByBlockSetting::default();
        let extra_headers =
            build_messages_extra_headers_with_retention(req.turn_metadata_header, needs_1h_beta);
        req.backend
            .execute_messages_turn(request, extra_headers)
            .await
    }

    /// The enterprise Copilot proxy exposes HTTP-SSE only. Returning
    /// `false` here prevents the client from probing WebSockets before
    /// falling back — saving the RTT the probe would cost.
    fn supports_websockets(&self) -> bool {
        false
    }

    /// Override per-slug context window + max output tokens from CAPI
    /// `/models` when available. Anthropic-direct family defaults
    /// assume the 1M-context beta SKU; CAPI caps Claude 4.6+ at 200K.
    /// Without this override, the TUI displays a 950K context window
    /// against a model whose actual prompt budget is ~168K — a real
    /// footgun on long sortie sessions.
    ///
    /// Falls through silently when ctx is unattached or the slug is
    /// missing from CAPI's catalog. Hard validation lives in
    /// [`Self::stream`].
    async fn populate_model_info(
        &self,
        info: &mut codex_protocol::openai_models::ModelInfo,
    ) -> codex_protocol::error::Result<()> {
        #[cfg(test)]
        if let Some(catalog) = self.test_catalog.as_ref() {
            Self::apply_capi_override(info, catalog);
            return Ok(());
        }
        // Apply per-slug overrides from whichever source is available:
        //   - Live CAPI catalog when the provider's ctx is already
        //     attached (set lazily by `ensure_ctx` on the first turn).
        //   - The hardcoded CAPI limits table as a deterministic
        //     fallback so the override fires at session-init time too
        //     — before any turn has minted a CAPI bearer. Without this
        //     fallback the TUI would display the Anthropic-direct 1M
        //     family default for Copilot-hosted Claude (rendered as
        //     950K after the 95% effective window discount) on the
        //     entire first turn.
        let catalog = match self.ctx() {
            Some(ctx) => ctx.models().await,
            None => std::sync::Arc::new(std::collections::HashMap::new()),
        };
        Self::apply_capi_override(info, &catalog);
        Ok(())
    }
}

/// Map XLI's `ReasoningEffortConfig` to the string CAPI accepts in
/// `output_config.effort` on the Anthropic Messages route.
///
/// CAPI accepts `"low"`, `"medium"`, `"high"`, and `"max"` on the Anthropic
/// route (exact allowed set is per-slug; sonnet-4.6 accepts all four).
/// `XHigh` maps to `"max"` — same semantic tier, different wire spelling
/// (verified live on CAPI `/models` 2026-07-02: sonnet-4.6 supports
/// `['low', 'medium', 'high', 'max']`, no `xhigh`).
/// `None` and `Minimal` collapse to `None` (no effort field sent).
/// The Responses route handles its own effort plumbing through
/// `core::client::stream_responses_api`.
///
/// Note: per-slug allow-lists (e.g. Opus 4.7 allows only `"medium"`)
/// are NOT enforced here — that validation is engagement 2 (CAPI
/// `/models` integration). Today, passing `"high"` for Opus 4.7 will
/// surface as a CAPI 400 with a clear error message.
fn effort_to_capi_str(
    effort: Option<codex_protocol::openai_models::ReasoningEffort>,
) -> Option<&'static str> {
    use codex_protocol::openai_models::ReasoningEffort;
    match effort? {
        ReasoningEffort::Low => Some("low"),
        ReasoningEffort::Medium => Some("medium"),
        ReasoningEffort::High => Some("high"),
        // XHigh and Max both map to "max" on the CAPI Anthropic route.
        // XHigh is the canonical internal tier; Max is the CAPI wire string.
        ReasoningEffort::XHigh | ReasoningEffort::Max => Some("max"),
        ReasoningEffort::None | ReasoningEffort::Minimal => None,
    }
}

#[cfg(test)]
mod tests {
    use codex_copilot_adapter::COPILOT_INTEGRATION_ID;
    use codex_copilot_adapter::EDITOR_VERSION;
    use codex_copilot_adapter::GITHUB_API_VERSION;
    use codex_copilot_adapter::X_GITHUB_API_VERSION;
    use pretty_assertions::assert_eq;

    use super::*;

    fn copilot_provider_info() -> ModelProviderInfo {
        ModelProviderInfo {
            wire_api: WireApi::Copilot,
            ..ModelProviderInfo::create_openai_provider(/*base_url*/ None)
        }
    }

    #[test]
    fn info_forwards_stored_model_provider_info() {
        let info = copilot_provider_info();
        let provider = CopilotModelProvider::new(info.clone(), /*auth_manager*/ None);
        assert_eq!(provider.info().wire_api, WireApi::Copilot);
        assert_eq!(provider.info().name, info.name);
    }

    #[test]
    fn auth_manager_is_none_when_constructed_without_one() {
        let provider =
            CopilotModelProvider::new(copilot_provider_info(), /*auth_manager*/ None);
        assert!(provider.auth_manager().is_none());
    }

    #[test]
    fn effective_wire_api_routes_claude_to_messages() {
        let provider =
            CopilotModelProvider::new(copilot_provider_info(), /*auth_manager*/ None);
        assert_eq!(
            provider.effective_wire_api("claude-sonnet-4.5"),
            WireApi::Messages
        );
    }

    #[test]
    fn effective_wire_api_routes_gpt5_to_responses() {
        let provider =
            CopilotModelProvider::new(copilot_provider_info(), /*auth_manager*/ None);
        // gpt-5.3-codex is in the CopilotEndpoints GPT5_RESPONSES_SLUGS
        // allowlist — unknown slugs fall through to ChatCompletions.
        assert_eq!(
            provider.effective_wire_api("gpt-5.3-codex"),
            WireApi::Responses
        );
    }

    #[test]
    fn effective_wire_api_falls_back_to_copilot_chat_completions() {
        let provider =
            CopilotModelProvider::new(copilot_provider_info(), /*auth_manager*/ None);
        // Unknown slugs ride the legacy chat-completions adapter path,
        // which we surface as `WireApi::Copilot` to the upstream
        // dispatch.
        assert_eq!(
            provider.effective_wire_api("some-legacy-model"),
            WireApi::Copilot
        );
    }

    #[test]
    fn extra_request_headers_stamps_all_four_shared_headers() {
        let provider =
            CopilotModelProvider::new(copilot_provider_info(), /*auth_manager*/ None);
        let headers = provider.extra_request_headers();

        assert_eq!(
            headers
                .get("copilot-integration-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default(),
            COPILOT_INTEGRATION_ID,
        );
        assert_eq!(
            headers
                .get("editor-version")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default(),
            EDITOR_VERSION,
        );
        assert_eq!(
            headers
                .get("x-initiator")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default(),
            "agent",
        );
        assert_eq!(
            headers
                .get(X_GITHUB_API_VERSION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default(),
            GITHUB_API_VERSION,
        );
    }

    #[test]
    fn supports_websockets_is_false() {
        let provider =
            CopilotModelProvider::new(copilot_provider_info(), /*auth_manager*/ None);
        assert!(
            !provider.supports_websockets(),
            "Copilot exposes HTTP-SSE only; probing WS wastes an RTT"
        );
    }
    #[test]
    fn effort_to_capi_str_maps_low_medium_high() {
        use codex_protocol::openai_models::ReasoningEffort;
        assert_eq!(effort_to_capi_str(Some(ReasoningEffort::Low)), Some("low"));
        assert_eq!(
            effort_to_capi_str(Some(ReasoningEffort::Medium)),
            Some("medium")
        );
        assert_eq!(
            effort_to_capi_str(Some(ReasoningEffort::High)),
            Some("high")
        );
    }

    #[test]
    fn effort_to_capi_str_maps_xhigh_and_max_to_max() {
        // XHigh and Max both produce "max" — verified against CAPI /models
        // 2026-07-02: claude-sonnet-4.6 reasoning_effort=['low','medium','high','max'].
        // CAPI rejects "xhigh" on the Anthropic route; "max" is the correct spelling.
        use codex_protocol::openai_models::ReasoningEffort;
        assert_eq!(effort_to_capi_str(Some(ReasoningEffort::XHigh)), Some("max"));
        assert_eq!(effort_to_capi_str(Some(ReasoningEffort::Max)), Some("max"));
    }

    #[test]
    fn effort_to_capi_str_drops_unsupported_variants() {
        use codex_protocol::openai_models::ReasoningEffort;
        assert_eq!(effort_to_capi_str(None), None);
        assert_eq!(effort_to_capi_str(Some(ReasoningEffort::None)), None);
        assert_eq!(effort_to_capi_str(Some(ReasoningEffort::Minimal)), None);
    }

    fn fake_capi_catalog() -> codex_copilot_adapter::CapiModelCatalog {
        use codex_copilot_adapter::CapiModelInfo;
        use codex_copilot_adapter::CapiModelLimits;
        use codex_copilot_adapter::CapiModelSupports;
        let mut map = std::collections::HashMap::new();
        map.insert(
            "claude-opus-4.7".to_string(),
            CapiModelInfo {
                id: "claude-opus-4.7".to_string(),
                name: Some("Claude Opus 4.7".to_string()),
                vendor: Some("Anthropic".to_string()),
                limits: CapiModelLimits {
                    max_context_window_tokens: Some(200_000),
                    max_prompt_tokens: Some(168_000),
                    max_output_tokens: Some(32_000),
                    max_non_streaming_output_tokens: Some(16_000),
                },
                supports: CapiModelSupports {
                    reasoning_effort: vec!["medium".to_string()],
                    adaptive_thinking: Some(true),
                    ..CapiModelSupports::default()
                },
            },
        );
        map.insert(
            "claude-opus-4.6".to_string(),
            CapiModelInfo {
                id: "claude-opus-4.6".to_string(),
                name: Some("Claude Opus 4.6".to_string()),
                vendor: Some("Anthropic".to_string()),
                limits: CapiModelLimits {
                    max_context_window_tokens: Some(200_000),
                    max_prompt_tokens: Some(168_000),
                    max_output_tokens: Some(32_000),
                    max_non_streaming_output_tokens: Some(16_000),
                },
                supports: CapiModelSupports {
                    reasoning_effort: vec![
                        "low".to_string(),
                        "medium".to_string(),
                        "high".to_string(),
                    ],
                    adaptive_thinking: Some(true),
                    ..CapiModelSupports::default()
                },
            },
        );
        std::sync::Arc::new(map)
    }

    fn make_model_info(slug: &str) -> codex_protocol::openai_models::ModelInfo {
        use codex_protocol::config_types::ReasoningSummary;
        use codex_protocol::openai_models::ConfigShellToolType;
        use codex_protocol::openai_models::InputModality;
        use codex_protocol::openai_models::ModelInfo;
        use codex_protocol::openai_models::ModelVisibility;
        use codex_protocol::openai_models::TruncationPolicyConfig;
        use codex_protocol::openai_models::WebSearchToolType;
        // Long-form struct construction: ModelInfo doesn't impl Default
        // and we want to keep this test self-contained (no dep on the
        // models-manager fallback path). Family-default 1M models the
        // pre-fix state of Claude 4.6+ slugs.
        ModelInfo {
            slug: slug.to_string(),
            display_name: slug.to_string(),
            description: None,
            default_reasoning_level: None,
            supported_reasoning_levels: Vec::new(),
            shell_type: ConfigShellToolType::Default,
            visibility: ModelVisibility::None,
            supported_in_api: true,
            priority: 99,
            additional_speed_tiers: Vec::new(),
            service_tiers: Vec::new(),
            default_service_tier: None,
            availability_nux: None,
            upgrade: None,
            base_instructions: String::new(),
            model_messages: None,
            supports_reasoning_summaries: false,
            default_reasoning_summary: ReasoningSummary::Auto,
            support_verbosity: false,
            default_verbosity: None,
            apply_patch_tool_type: None,
            web_search_tool_type: WebSearchToolType::Text,
            truncation_policy: TruncationPolicyConfig::bytes(10_000),
            supports_parallel_tool_calls: true,
            supports_image_detail_original: false,
            context_window: Some(1_000_000),
            max_context_window: Some(1_000_000),
            auto_compact_token_limit: None,
            effective_context_window_percent: 95,
            experimental_supported_tools: Vec::new(),
            input_modalities: vec![InputModality::Text],
            used_fallback_model_metadata: true,
            supports_search_tool: false,
        }
    }

    #[tokio::test]
    async fn populate_model_info_overrides_context_window_from_capi() {
        let provider = CopilotModelProvider::new(copilot_provider_info(), None)
            .with_test_catalog(fake_capi_catalog());
        let mut info = make_model_info("claude-opus-4.7");
        provider.populate_model_info(&mut info).await.unwrap();
        assert_eq!(
            info.context_window,
            Some(168_000),
            "CAPI max_prompt_tokens should override the family-default 1M"
        );
        assert_eq!(
            info.max_context_window,
            Some(200_000),
            "CAPI max_context_window_tokens should override the family-default 1M"
        );
        assert!(
            !info.used_fallback_model_metadata,
            "live data was applied; fallback marker should be cleared"
        );
    }

    #[tokio::test]
    async fn populate_model_info_no_op_when_slug_missing() {
        // Slug not in CAPI catalog (e.g. legacy gpt-4o on the chat-completions
        // fallback path) — provider must not touch the family defaults.
        let provider = CopilotModelProvider::new(copilot_provider_info(), None)
            .with_test_catalog(fake_capi_catalog());
        let mut info = make_model_info("gpt-4o-legacy-not-on-capi");
        provider.populate_model_info(&mut info).await.unwrap();
        assert_eq!(info.context_window, Some(1_000_000));
        assert!(info.used_fallback_model_metadata);
    }

    #[tokio::test]
    async fn populate_model_info_no_op_when_ctx_unattached() {
        // Even when the lazy CAPI ctx hasn't been attached yet,
        // hardcoded CAPI limits must still apply at session-init time
        // — otherwise the TUI displays the Anthropic-direct 1M family
        // default (950K after the 95% effective window discount) for
        // Copilot-hosted Claude on the entire first turn.
        // Slug not in the hardcoded table falls through unchanged.
        let provider = CopilotModelProvider::new(copilot_provider_info(), None);
        let mut info = make_model_info("claude-opus-4.7");
        provider.populate_model_info(&mut info).await.unwrap();
        assert_eq!(
            info.context_window,
            Some(168_000),
            "hardcoded CAPI limits must override the 1M family default"
        );
        assert_eq!(info.max_context_window, Some(200_000));
        assert!(!info.used_fallback_model_metadata);

        // Unknown slug (not in hardcoded table, no live catalog) ->
        // family defaults preserved.
        let mut unknown = make_model_info("legacy-model-not-on-capi");
        provider.populate_model_info(&mut unknown).await.unwrap();
        assert_eq!(unknown.context_window, Some(1_000_000));
        assert!(unknown.used_fallback_model_metadata);
    }

    /// Pin the output-cap override. Family default for Claude Opus is
    /// 128K output, but CAPI caps opus-4.7 at 32K and opus-4.8 at 64K.
    /// Without this, the Anthropic Messages request goes out with
    /// `max_tokens=128000` — CAPI is lenient today, but the value
    /// would mis-inform any client-side budgeting and could 400 if
    /// CAPI starts enforcing.
    #[tokio::test]
    async fn populate_model_info_overrides_max_output_tokens_from_hardcoded_table() {
        let provider = CopilotModelProvider::new(copilot_provider_info(), None);

        let mut opus47 = make_model_info("claude-opus-4.7");
        ProviderCaps::merge_override(
            "claude-opus-4.7",
            ProviderCapsPatch {
                max_output_tokens: Some(Some(128_000)),
                ..Default::default()
            },
        );
        provider.populate_model_info(&mut opus47).await.unwrap();
        assert_eq!(
            ProviderCaps::for_model("claude-opus-4.7").max_output_tokens,
            Some(32_000)
        );

        let mut opus48 = make_model_info("claude-opus-4.8");
        ProviderCaps::merge_override(
            "claude-opus-4.8",
            ProviderCapsPatch {
                max_output_tokens: Some(Some(128_000)),
                ..Default::default()
            },
        );
        provider.populate_model_info(&mut opus48).await.unwrap();
        assert_eq!(
            ProviderCaps::for_model("claude-opus-4.8").max_output_tokens,
            Some(64_000)
        );

        let mut haiku = make_model_info("claude-haiku-4.5");
        ProviderCaps::merge_override(
            "claude-haiku-4.5",
            ProviderCapsPatch {
                max_output_tokens: Some(Some(8_192)),
                ..Default::default()
            },
        );
        provider.populate_model_info(&mut haiku).await.unwrap();
        assert_eq!(
            ProviderCaps::for_model("claude-haiku-4.5").max_output_tokens,
            Some(64_000),
            "haiku CAPI cap is 64K, larger than the 8K family default"
        );
    }
}

#[cfg(test)]
mod force_refresh_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn test_provider() -> CopilotModelProvider {
        CopilotModelProvider::new(
            ModelProviderInfo {
                wire_api: WireApi::Copilot,
                ..ModelProviderInfo::create_openai_provider(None)
            },
            None,
        )
    }

    #[test]
    fn force_refresh_attempts_starts_at_zero() {
        let provider = test_provider();
        assert_eq!(
            provider.force_refresh_attempts.load(Ordering::Acquire),
            0,
            "fresh provider must start with a zero-budget counter"
        );
    }

    #[tokio::test]
    async fn try_refresh_auth_returns_fatal_after_cap() {
        let provider = test_provider();
        // Pre-load the counter to the cap.
        provider
            .force_refresh_attempts
            .store(COPILOT_MAX_FORCE_REFRESHES_PER_SESSION, Ordering::Release);

        let result = provider.try_refresh_auth().await;
        assert!(result.is_err(), "budget exhausted — must fail");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("401 persists"),
            "error must mention 401: {msg}"
        );
        // Counter should be clamped at the cap, not incremented past it.
        assert_eq!(
            provider.force_refresh_attempts.load(Ordering::Acquire),
            COPILOT_MAX_FORCE_REFRESHES_PER_SESSION,
            "counter must saturate at cap, not grow unboundedly"
        );
    }

    #[test]
    fn note_request_succeeded_resets_counter() {
        let provider = test_provider();
        provider.force_refresh_attempts.store(2, Ordering::Release);
        provider.note_request_succeeded();
        assert_eq!(
            provider.force_refresh_attempts.load(Ordering::Acquire),
            0,
            "successful request must reset the per-session counter"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn force_refresh_budget_shared_across_clones() {
        // The provider is behind Arc<dyn ModelProvider> and gets cloned.
        // Arc<AtomicU32> means clones share the budget counter — this
        // test locks that invariant down.
        let provider = std::sync::Arc::new(test_provider());
        let cap = COPILOT_MAX_FORCE_REFRESHES_PER_SESSION;
        let total: u32 = cap + 3;

        let mut handles = Vec::with_capacity(total as usize);
        for _ in 0..total {
            let p = std::sync::Arc::clone(&provider);
            handles.push(tokio::spawn(async move {
                let prior = p.force_refresh_attempts.fetch_add(1, Ordering::AcqRel);
                prior < COPILOT_MAX_FORCE_REFRESHES_PER_SESSION
            }));
        }

        let mut permitted = 0u32;
        let mut bailed = 0u32;
        for h in handles {
            if h.await.expect("task panicked") {
                permitted += 1;
            } else {
                bailed += 1;
            }
        }

        assert_eq!(
            permitted, cap,
            "exactly CAP concurrent 401s must be permitted (got {permitted}/{bailed}, CAP={cap})"
        );
        assert_eq!(bailed, 3);

        // Reset works across the shared counter.
        provider.note_request_succeeded();
        assert_eq!(
            provider.force_refresh_attempts.load(Ordering::Acquire),
            0,
            "note_request_succeeded must zero the shared counter"
        );
    }
}

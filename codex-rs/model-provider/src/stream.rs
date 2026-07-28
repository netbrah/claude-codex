//! Provider streaming surface (Sortie 1 seam).
//!
//! This module declares the request shape, return type, and transport
//! backend trait that [`crate::ModelProvider::stream`] exchanges with a
//! provider. The trait method itself lives on [`crate::ModelProvider`]
//! so impls can override it alongside the other provider policy hooks.
//!
//! # Why this exists
//!
//! Historically `codex-core` dispatched on `WireApi` inside
//! `core/src/client.rs::ModelClientSession::stream` to reach three wire
//! implementations: OpenAI `/responses`, Anthropic `/messages`, and
//! Copilot. The per-wire branches carried ~580 LoC of XLI-specific
//! logic in the highest-churn upstream file in the tree.
//!
//! Sortie 1 moves the Messages and Copilot dispatch out of `core` and
//! behind new sibling provider crates that implement [`ModelProvider`].
//! Once [`ModelProvider::stream`] exists, `core/src/client.rs` collapses
//! its Messages and Copilot arms to `self.state.provider.stream(req)` —
//! and `core` stops having to know how each wire streams.
//!
//! # Why the request is its own type
//!
//! Passing the streaming parameters as a struct (rather than as a long
//! positional argument list) is required by XLI's engine conventions
//! (`AGENTS.md` §"Upstream Engine Conventions": "Avoid bool or ambiguous
//! `Option` parameters that force callers to write hard-to-read code").
//! It also lets later commits add fields without breaking any downstream
//! provider crate's trait impl.
//!
//! # Separation of concerns — backend trait
//!
//! The provider crate (e.g. `codex-provider-anthropic`) owns **what a
//! wire-specific request looks like**: the Anthropic `MessagesApiRequest`
//! construction, tool-schema translation, cache-control placement,
//! reasoning-effort → thinking-param mapping, etc.
//!
//! The **orchestration** — 401 retry loop, per-attempt
//! `current_client_setup` resolution, Copilot base-url splicing,
//! telemetry composition, `ApiMessagesClient` construction, auth recovery
//! — is wire-agnostic HTTP transport that stays in `codex-core`.
//!
//! The boundary is a one-method backend trait ([`MessagesBackend`]) that
//! core implements and the provider calls. The provider builds the
//! typed request payload; the backend runs the turn. Neither side knows
//! the other's internals.
//!
//! This is the "hybrid" shape settled on in the Sortie 1 pre-flight:
//! fat-trait ("MessagesOrchestrator") would have exposed 15+ core
//! internals (telemetry types, auth-recovery state machines, transport
//! errors, inference-trace handles) to the provider crate — 600 LoC of
//! plumbing for 90 LoC of actual wire-specific carve. The hybrid cut
//! lands both at the right weight: ~90 LoC of pure request build in
//! the provider crate + ~1 trait method across the seam.

use async_trait::async_trait;
use codex_api::MessagesApiRequest;
use codex_prompt::Prompt;
use codex_prompt::ResponseStream;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::config_types::ToolChoice;
use codex_protocol::error::Result;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use http::HeaderMap;


/// Anthropic prompt-cache retention for one turn (local mirror of
/// `codex_config::types::CacheRetention`; duplicated to keep this
/// crate leaf-safe and free of a `codex-config` dep).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CacheRetentionSetting {
    /// 5-minute prompt-cache TTL (`{"type":"ephemeral"}`).
    #[default]
    Ephemeral,
    /// Extended 1-hour TTL (`{"type":"ephemeral","ttl":"1h"}`).
    /// Degraded to `Ephemeral` by the provider if the model does not
    /// support it (`ProviderCaps::supports_1h_cache == false`).
    OneHour,
}

/// Per-anchor cache retention overrides (local mirror of
/// `codex_config::types::CacheRetentionByBlock`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheRetentionByBlockSetting {
    pub system: Option<CacheRetentionSetting>,
    pub tool: Option<CacheRetentionSetting>,
    pub user_last: Option<CacheRetentionSetting>,
    pub user_previous: Option<CacheRetentionSetting>,
}

impl CacheRetentionByBlockSetting {
    /// Resolve the retention for a named anchor, falling back to `default`.
    pub fn resolve_system(&self, default: CacheRetentionSetting) -> CacheRetentionSetting {
        self.system.unwrap_or(default)
    }
    pub fn resolve_tool(&self, default: CacheRetentionSetting) -> CacheRetentionSetting {
        self.tool.unwrap_or(default)
    }
    pub fn resolve_user_last(&self, default: CacheRetentionSetting) -> CacheRetentionSetting {
        self.user_last.unwrap_or(default)
    }
    pub fn resolve_user_previous(&self, default: CacheRetentionSetting) -> CacheRetentionSetting {
        self.user_previous.unwrap_or(default)
    }
}

/// Parameters passed to [`crate::ModelProvider::stream`].
///
/// Held by reference for the lifetime of a single call — callers keep
/// ownership of the prompt and any borrowed fields. Construct with
/// struct-literal syntax: `#[non_exhaustive]` guarantees new fields
/// added in later commits don't break the trait impl in any provider
/// crate, provided callers re-construct the request rather than mutate
/// an existing one.
///
/// # Field set (Sortie 1 Commit 5b)
///
/// The field set below is driven by what
/// `core::ModelClientSession::stream_messages_api` actually consumes —
/// worked backward from that signature during the pre-flight carve.
/// Copilot may need additional fields in Commit 5c; add them here and
/// update the Anthropic provider to ignore what it doesn't use.
#[non_exhaustive]
pub struct ProviderStreamRequest<'a> {
    /// The rendered prompt to stream.
    pub prompt: &'a Prompt,
    /// Resolved model metadata (slug, input modalities, context window).
    pub model_info: &'a ModelInfo,
    /// Reasoning-effort config selected for this turn (adaptive
    /// thinking on Anthropic, reasoning-effort on Responses).
    pub effort: Option<ReasoningEffortConfig>,
    /// Reasoning-summary config. Currently Responses-only; the
    /// Anthropic provider ignores this but it's passed for symmetry.
    pub summary: ReasoningSummaryConfig,
    /// Service tier. Currently Responses-only; Anthropic ignores it.
    pub service_tier: Option<ServiceTier>,
    /// Sampling temperature. `0.0` produces deterministic output.
    /// Field broken out from `codex_config::types::SamplingParams` to
    /// keep `codex-model-provider` leaf-safe (no `codex-config` dep).
    pub temperature: Option<f64>,
    /// Nucleus sampling threshold (0.0–1.0).
    pub top_p: Option<f64>,
    /// Top-k sampling (Anthropic Messages API only).
    pub top_k: Option<u32>,
    /// Optional raw value for the `x-codex-turn-metadata` header. Every
    /// wire supports it when present.
    pub turn_metadata_header: Option<&'a str>,
    /// Tool-choice policy resolved for this turn.
    pub tool_choice: Option<&'a ToolChoice>,
    /// Optional stable user identifier to include in Anthropic
    /// `metadata.user_id`. Configured via `messages_metadata_user_id`
    /// in `ModelClient` state. Anthropic-wire only.
    pub messages_metadata_user_id: Option<&'a str>,
    /// Whether the session has web search enabled.
    ///
    /// For Gemini, this gates `googleSearch` grounding tool injection;
    /// each provider crate interprets this flag according to its own
    /// wire capabilities. Defaults to `false`.
    pub web_search_enabled: bool,
    /// Anthropic /messages cache_control retention (Anthropic-wire only).
    /// Responses and Gemini providers ignore this field.
    pub cache_retention: CacheRetentionSetting,
    /// Per-anchor cache_control retention overrides (Anthropic-wire only).
    pub cache_retention_by_block: CacheRetentionByBlockSetting,
    /// Top-level `output_config.effort` fallback for Anthropic models.
    /// Overridden per-turn by `effort`. `None` means use provider default.
    pub model_effort_default: Option<&'a str>,
    /// Backend used by the provider to actually run the turn. Core
    /// implements this; the provider calls it after building its
    /// wire-specific request payload.
    pub backend: &'a dyn MessagesBackend,
}

impl<'a> ProviderStreamRequest<'a> {
    /// Starts building a request. Required fields are positional; optional
    /// fields become `.with_*` chained setters. Returns a
    /// [`ProviderStreamRequestBuilder`] with the remaining fields
    /// defaulted to `None` / `ReasoningSummaryConfig::default()`.
    #[must_use]
    pub fn builder(
        prompt: &'a Prompt,
        model_info: &'a ModelInfo,
        backend: &'a dyn MessagesBackend,
    ) -> ProviderStreamRequestBuilder<'a> {
        ProviderStreamRequestBuilder {
            prompt,
            model_info,
            backend,
            effort: None,
            summary: ReasoningSummaryConfig::default(),
            service_tier: None,
            temperature: None,
            top_p: None,
            top_k: None,
            turn_metadata_header: None,
            tool_choice: None,
            messages_metadata_user_id: None,
            web_search_enabled: false,
            cache_retention: CacheRetentionSetting::Ephemeral,
            cache_retention_by_block: CacheRetentionByBlockSetting::default(),
            model_effort_default: None,
        }
    }
}

/// Builder for [`ProviderStreamRequest`]. External callers must go
/// through this type because `#[non_exhaustive]` prohibits
/// struct-literal construction from outside `codex-model-provider`.
///
/// Adding a field to `ProviderStreamRequest` means: (1) add it here,
/// (2) add a `.with_*` chainable setter, (3) forward it in
/// [`ProviderStreamRequestBuilder::build`]. That shape keeps every
/// downstream call site forward-compatible with the only churn being
/// at the single construction site that now wants to pass the new
/// field.
pub struct ProviderStreamRequestBuilder<'a> {
    prompt: &'a Prompt,
    model_info: &'a ModelInfo,
    backend: &'a dyn MessagesBackend,
    effort: Option<ReasoningEffortConfig>,
    summary: ReasoningSummaryConfig,
    service_tier: Option<ServiceTier>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<u32>,
    turn_metadata_header: Option<&'a str>,
    tool_choice: Option<&'a ToolChoice>,
    messages_metadata_user_id: Option<&'a str>,
    web_search_enabled: bool,
    cache_retention: CacheRetentionSetting,
    cache_retention_by_block: CacheRetentionByBlockSetting,
    model_effort_default: Option<&'a str>,
}

impl<'a> ProviderStreamRequestBuilder<'a> {
    #[must_use]
    pub fn with_effort(mut self, effort: Option<ReasoningEffortConfig>) -> Self {
        self.effort = effort;
        self
    }

    #[must_use]
    pub fn with_summary(mut self, summary: ReasoningSummaryConfig) -> Self {
        self.summary = summary;
        self
    }

    #[must_use]
    pub fn with_service_tier(mut self, service_tier: Option<ServiceTier>) -> Self {
        self.service_tier = service_tier;
        self
    }

    #[must_use]
    pub fn with_sampling(
        mut self,
        temperature: Option<f64>,
        top_p: Option<f64>,
        top_k: Option<u32>,
    ) -> Self {
        self.temperature = temperature;
        self.top_p = top_p;
        self.top_k = top_k;
        self
    }

    #[must_use]
    pub fn with_turn_metadata_header(mut self, header: Option<&'a str>) -> Self {
        self.turn_metadata_header = header;
        self
    }

    #[must_use]
    pub fn with_tool_choice(mut self, tool_choice: Option<&'a ToolChoice>) -> Self {
        self.tool_choice = tool_choice;
        self
    }

    #[must_use]
    pub fn with_messages_metadata_user_id(mut self, user_id: Option<&'a str>) -> Self {
        self.messages_metadata_user_id = user_id;
        self
    }

    #[must_use]
    pub fn with_web_search_enabled(mut self, enabled: bool) -> Self {
        self.web_search_enabled = enabled;
        self
    }


    #[must_use]
    pub fn with_cache_retention(mut self, retention: CacheRetentionSetting) -> Self {
        self.cache_retention = retention;
        self
    }

    #[must_use]
    pub fn with_cache_retention_by_block(mut self, by_block: CacheRetentionByBlockSetting) -> Self {
        self.cache_retention_by_block = by_block;
        self
    }

    #[must_use]
    pub fn with_model_effort_default(mut self, effort: Option<&'a str>) -> Self {
        self.model_effort_default = effort;
        self
    }

    /// Finalizes the builder. `#[non_exhaustive]` permits struct-literal
    /// construction inside this crate; external callers reach it only
    /// through this method, so adding new fields to the request stays
    /// non-breaking.
    #[must_use]
    pub fn build(self) -> ProviderStreamRequest<'a> {
        ProviderStreamRequest {
            prompt: self.prompt,
            model_info: self.model_info,
            effort: self.effort,
            summary: self.summary,
            service_tier: self.service_tier,
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            turn_metadata_header: self.turn_metadata_header,
            tool_choice: self.tool_choice,
            messages_metadata_user_id: self.messages_metadata_user_id,
            web_search_enabled: self.web_search_enabled,
            cache_retention: self.cache_retention,
            cache_retention_by_block: self.cache_retention_by_block,
            model_effort_default: self.model_effort_default,
            backend: self.backend,
        }
    }
}

/// Backend surface a provider invokes to actually run a Messages-wire
/// turn. Core implements this on an adapter that owns the 401 retry
/// loop, per-attempt `current_client_setup` resolution, Copilot base-url
/// splicing, telemetry composition, and `ApiMessagesClient` dispatch.
///
/// The method runs the **entire** turn, not a single HTTP attempt — 401
/// retries are internal. The provider hands in a prebuilt
/// [`MessagesApiRequest`] and any extra headers; it gets back a
/// [`ResponseStream`] ready to forward to the session loop, or an error.
///
/// # Why one method, not many
///
/// Every internal detail of the orchestration loop (`CurrentClientSetup`,
/// `SessionTelemetry`, `PendingUnauthorizedRetry`, `InferenceTraceAttempt`,
/// `AuthRequestTelemetryContext`, the `handle_unauthorized_for_copilot`
/// callback protocol, `map_response_stream`, `map_api_error`) is a core
/// internal. Exposing them through the trait would distribute coupling
/// without buying isolation — the provider crate would transitively
/// know core's auth-recovery state machine, telemetry taxonomy, and
/// transport-error classification. One-method backend keeps the seam
/// narrow and the sides honest.
#[async_trait]
pub trait MessagesBackend: Send + Sync {
    /// Runs a full Messages-wire turn: dispatches the request through
    /// core's transport layer, handles 401 retries, applies Copilot
    /// splices when the session provider is Copilot, and returns a
    /// stream of response events.
    async fn execute_messages_turn(
        &self,
        request: MessagesApiRequest,
        extra_headers: HeaderMap,
    ) -> Result<ResponseStream>;
}

/// Alias for the stream returned by [`crate::ModelProvider::stream`].
///
/// Re-exported so provider crates can implement the trait without taking
/// a separate dep on `codex-prompt` just for the return type.
pub type ProviderResponseStream = ResponseStream;

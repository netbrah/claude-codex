//! `AnthropicMessagesProvider` — the Sortie 1 provider that owns the
//! Anthropic `/messages` wire.
//!
//! # Status (Sortie 1, through Commit 5b)
//!
//! What this provider does:
//!
//! * Stores the underlying `ModelProviderInfo` + optional `AuthManager`.
//! * Forwards `info`, `auth_manager`, and `auth` exactly like
//!   `ConfiguredModelProvider`.
//! * Overrides [`ModelProvider::effective_wire_api`] to auto-upgrade to
//!   the Responses wire for non-Anthropic model slugs
//!   (see [`crate::is_anthropic_model`]).
//! * **Commit 5b:** overrides [`ModelProvider::stream`] to build a
//!   well-typed `MessagesApiRequest` from the per-turn inputs (see
//!   [`crate::request::build_messages_request`]) and delegate to the
//!   caller-supplied [`codex_model_provider::MessagesBackend`], which
//!   owns the 401 retry loop, `current_client_setup` resolution,
//!   Copilot base-url splice, telemetry composition, and
//!   `ApiMessagesClient` dispatch.
//!
//! # Why the split
//!
//! The 185 LoC of the legacy `stream_messages_api` in `codex-core`
//! broke into two cleanly separable concerns:
//!
//! - Wire-specific request construction: `MessagesApiRequest` shape,
//!   `ResponseItem[]` → Anthropic `messages[]` translation, tool
//!   schema, system-block assembly with cache-control, reasoning
//!   effort → thinking-param mapping.
//! - Wire-agnostic orchestration: 401 retry, auth recovery,
//!   telemetry, transport construction, response-stream mapping.
//!
//! The first is Anthropic-specific and belongs here. The second is
//! transport orchestration that applies to any wire and belongs in
//! `codex-core`. The [`codex_model_provider::MessagesBackend`] trait
//! carries the seam — one method, two well-typed arguments.
//!
//! A "fat trait" alternative (call it `MessagesOrchestrator`) would
//! have exposed 15+ core internals (telemetry types, auth-recovery
//! state machines, transport errors, inference-trace handles) through
//! the trait and let the provider drive the loop. That would have been
//! ~600 LoC of plumbing for ~90 LoC of actual wire-specific carve —
//! and would have silently distributed core's internals across the
//! provider crate. The one-method backend keeps the boundary narrow.

use std::path::PathBuf;
use std::sync::Arc;

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
use codex_protocol::error::Result;
use codex_protocol::openai_models::ModelsResponse;

use crate::model::is_anthropic_model;
use crate::request::Sampling;
use crate::request::build_messages_extra_headers_with_retention;
use crate::request::build_messages_request;
use codex_model_provider::CacheRetentionByBlockSetting;
use codex_model_provider::CacheRetentionSetting;

/// Runtime provider for the Anthropic `/messages` wire.
///
/// Constructed with a pre-resolved `ModelProviderInfo` whose
/// `wire_api == WireApi::Messages` and an optional `AuthManager` (the
/// registry owns the auth-manager construction logic; this crate stays
/// agnostic). Held behind `Arc<dyn ModelProvider>` like every other
/// provider.
#[derive(Clone, Debug)]
pub struct AnthropicMessagesProvider {
    info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
}

impl AnthropicMessagesProvider {
    /// Constructs an `AnthropicMessagesProvider` from pre-resolved inputs.
    ///
    /// The caller — typically `codex-model-provider`'s registry in
    /// Commit 5 — is responsible for ensuring `info.wire_api` is
    /// `WireApi::Messages` and for constructing `auth_manager` the same
    /// way it does for the configured OpenAI provider (Messages wire
    /// reuses the OpenAI-style auth flow).
    #[must_use]
    pub fn new(info: ModelProviderInfo, auth_manager: Option<Arc<AuthManager>>) -> Self {
        Self { info, auth_manager }
    }
}

#[async_trait::async_trait]
impl ModelProvider for AnthropicMessagesProvider {
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

    /// Auto-upgrades to the Responses wire for non-Anthropic model slugs.
    ///
    /// When a caller configures a provider with `wire_api = "messages"`
    /// but runs a non-Anthropic model through it (e.g. `gpt-5.3-codex`
    /// routed through a LiteLLM proxy), LiteLLM double-translates
    /// Messages → Responses → Messages and its stream adapter drops
    /// reasoning summary events (LiteLLM 1.82.x). We keep Messages
    /// only for Anthropic slugs; everything else rides `/v1/responses`
    /// which every OpenAI-compatible proxy supports natively on the
    /// same base URL.
    ///
    /// This mirrors the free-function `effective_wire_api` that used to
    /// live in `codex-core::client.rs`; Commit 5 (or 6) deletes that
    /// call site in favor of `provider.effective_wire_api()`.
    fn effective_wire_api(&self, model_slug: &str) -> WireApi {
        if is_anthropic_model(model_slug) {
            WireApi::Messages
        } else {
            tracing::debug!(
                model = model_slug,
                "auto-upgrading wire API from Messages to Responses for non-Anthropic model"
            );
            WireApi::Responses
        }
    }

    /// Overrides the `Err(UnsupportedOperation)` default from
    /// [`codex_model_provider::ModelProvider::stream`].
    ///
    /// Builds the wire-specific `MessagesApiRequest` + extra HTTP
    /// headers from `req` and hands them to the caller-supplied
    /// [`codex_model_provider::MessagesBackend`], which runs the turn
    /// (including 401 retries, Copilot base-url splicing, telemetry,
    /// and response-stream mapping). The provider never touches the
    /// transport layer directly.
    ///
    /// The request shape produced here is byte-identical to the
    /// in-core `stream_messages_api` path that preceded it — the body
    /// of that function was the pre-lift source, and the
    /// request-build block has been moved verbatim with the only
    /// changes being the tool-choice / metadata plumbing that came in
    /// through [`ProviderStreamRequest`] instead of through
    /// `ModelClient` state.
    async fn stream<'a>(
        &'a self,
        req: ProviderStreamRequest<'a>,
    ) -> Result<ProviderResponseStream> {
        let sampling = Sampling {
            temperature: req.temperature,
            top_p: req.top_p,
            top_k: req.top_k,
        };
        let request = build_messages_request(
            req.prompt,
            req.model_info,
            req.effort,
            sampling,
            req.tool_choice,
            req.messages_metadata_user_id,
            /*output_effort_override*/ None,
            req.cache_retention,
            req.cache_retention_by_block.clone(),
            req.model_effort_default,
        );
        // Inject the extended-cache-ttl beta header when 1h retention is
        // active on a model that supports it.
        let needs_1h_beta = req.cache_retention == CacheRetentionSetting::OneHour
            || req.cache_retention_by_block != CacheRetentionByBlockSetting::default();
        let extra_headers = build_messages_extra_headers_with_retention(
            req.turn_metadata_header,
            needs_1h_beta,
        );
        req.backend
            .execute_messages_turn(request, extra_headers)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages_provider_info() -> ModelProviderInfo {
        ModelProviderInfo {
            wire_api: WireApi::Messages,
            ..ModelProviderInfo::create_openai_provider(/*base_url*/ None)
        }
    }

    #[test]
    fn info_forwards_stored_model_provider_info() {
        let info = messages_provider_info();
        let provider = AnthropicMessagesProvider::new(info.clone(), /*auth_manager*/ None);
        assert_eq!(provider.info().name, info.name);
        assert_eq!(provider.info().wire_api, WireApi::Messages);
    }

    #[test]
    fn auth_manager_is_none_when_constructed_without_one() {
        let provider =
            AnthropicMessagesProvider::new(messages_provider_info(), /*auth_manager*/ None);
        assert!(provider.auth_manager().is_none());
    }

    #[test]
    fn effective_wire_api_keeps_messages_for_claude_slugs() {
        let provider =
            AnthropicMessagesProvider::new(messages_provider_info(), /*auth_manager*/ None);
        assert_eq!(
            provider.effective_wire_api("claude-sonnet-4-6"),
            WireApi::Messages
        );
        assert_eq!(
            provider.effective_wire_api("claude-opus-4-6"),
            WireApi::Messages
        );
        // Vertex AI variant — still recognized as Anthropic.
        assert_eq!(
            provider.effective_wire_api("claude-sonnet-4-6@default"),
            WireApi::Messages
        );
    }

    #[test]
    fn effective_wire_api_upgrades_non_anthropic_slugs_to_responses() {
        let provider =
            AnthropicMessagesProvider::new(messages_provider_info(), /*auth_manager*/ None);
        assert_eq!(
            provider.effective_wire_api("gpt-5.3-codex"),
            WireApi::Responses
        );
        assert_eq!(provider.effective_wire_api("o3-mini"), WireApi::Responses);
        assert_eq!(
            provider.effective_wire_api("custom-model"),
            WireApi::Responses
        );
    }

    #[test]
    fn other_extension_hooks_keep_default_behavior() {
        // supports_websockets forwards ModelProviderInfo's value;
        // extra_request_headers returns the empty map. The Anthropic
        // provider does not override these — Commit 3c is tightly
        // scoped to effective_wire_api only.
        let provider =
            AnthropicMessagesProvider::new(messages_provider_info(), /*auth_manager*/ None);
        assert_eq!(
            provider.supports_websockets(),
            provider.info().supports_websockets
        );
        assert!(provider.extra_request_headers().is_empty());
    }
}

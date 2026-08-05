//! Runtime provider registry — constructs a concrete
//! [`SharedModelProvider`] from configured `ModelProviderInfo`.
//!
//! # Architecture (Sortie 1 Commit 5a)
//!
//! This crate sits one layer above the provider impl crates:
//!
//! ```text
//! codex-model-provider                           (trait + defaults)
//!     ↑                         ↑
//! codex-provider-anthropic   codex-provider-copilot
//!     ↑                         ↑
//!          codex-provider-registry                (THIS CRATE)
//!                  ↑
//! codex-core, codex-cli, codex-models-manager, ...
//! ```
//!
//! The registry is a leaf above the impl crates — each impl crate knows
//! about its own wire and the trait, and only the registry knows the
//! full set of impls. This honours dependency inversion: adding a new
//! provider (e.g. Gemini in Sortie 5) means adding one crate and one
//! `match` arm here, with no churn in `codex-model-provider`,
//! `codex-core`, or any other impl crate.
//!
//! # Dispatch order
//!
//! [`create_model_provider`] dispatches in this order:
//!
//! 1. [`ModelProviderInfo::is_amazon_bedrock`] — SigV4 path, independent
//!    of `WireApi`.
//! 2. Otherwise, branch on [`ModelProviderInfo::wire_api`]:
//!    - [`WireApi::Messages`] → [`AnthropicMessagesProvider`]
//!    - [`WireApi::Copilot`] → [`CopilotModelProvider`]
//!    - [`WireApi::Responses`] → [`ConfiguredModelProvider`]
//!
//! All non-Bedrock branches run [`auth_manager_for_provider`] first so
//! command-backed external auth still overrides the caller's manager
//! when configured.
//!
//! # What this commit does **not** change
//!
//! The Messages and Copilot providers inherit the
//! `Err(UnsupportedOperation)` default for
//! [`codex_model_provider::ModelProvider::stream`] — `codex-core` still
//! owns `stream_messages_api` and `stream_copilot_api` and dispatches
//! to them via its in-file `match wire_api` in `client.rs::stream()`.
//! Swapping what the `SharedModelProvider` arc holds is therefore zero
//! behavior change at runtime. Commit 5b lifts the orchestration into
//! the provider crates, overrides `stream`, and flips the dispatch.

use std::sync::Arc;

use codex_login::AuthManager;
use codex_model_provider::AmazonBedrockModelProvider;
use codex_model_provider::ConfiguredModelProvider;
use codex_model_provider::ModelProvider;
use codex_model_provider::SharedModelProvider;
use codex_model_provider::auth_manager_for_provider;
use codex_model_provider_info::ModelProviderAwsAuthInfo;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::WireApi;
use codex_protocol::openai_models::ModelInfo;
use codex_provider_anthropic::AnthropicMessagesProvider;
use codex_provider_copilot::CopilotModelProvider;
use codex_provider_gemini::GeminiModelProvider;

/// Creates the runtime model provider matching the given metadata.
///
/// See the module-level docs for the full dispatch order and the
/// architectural rationale for this crate's existence.
pub fn create_model_provider(
    provider_info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
) -> SharedModelProvider {
    if provider_info.is_amazon_bedrock() {
        let aws = provider_info
            .aws
            .clone()
            .unwrap_or(ModelProviderAwsAuthInfo {
                profile: None,
                region: None,
            });
        return Arc::new(AmazonBedrockModelProvider {
            info: provider_info,
            aws,
        });
    }

    let auth_manager = auth_manager_for_provider(auth_manager, &provider_info);
    match provider_info.wire_api {
        WireApi::Messages => Arc::new(AnthropicMessagesProvider::new(provider_info, auth_manager)),
        WireApi::Copilot => Arc::new(CopilotModelProvider::new(provider_info, auth_manager)),
        WireApi::Responses => Arc::new(ConfiguredModelProvider::new(provider_info, auth_manager)),
        WireApi::GenerateContent => Arc::new(GeminiModelProvider::new(provider_info, auth_manager)),
    }
}

/// Enrich a `ModelInfo` with backend-specific per-slug metadata.
///
/// Constructs the runtime provider for `provider_info` and invokes its
/// [`ModelProvider::populate_model_info`] hook. This is the single
/// integration point that lets a provider crate override the static
/// model catalog's family-default heuristics with backend-specific
/// per-slug data.
///
/// # No-op contract for non-Copilot wires
///
/// Today only [`CopilotModelProvider`] overrides
/// `populate_model_info`. Every other provider
/// ([`AnthropicMessagesProvider`] for `llm_proxy_messages`,
/// [`ConfiguredModelProvider`] for `responses` / `llm_proxy_responses`,
/// [`GeminiModelProvider`], [`AmazonBedrockModelProvider`]) inherits
/// the trait default `Ok(())` and leaves `info` untouched. That is a
/// **deliberate** design choice: LiteLLM-style enrichment for the
/// `llm_proxy_*` wires already happens *inside*
/// `models_manager.get_model_info()` (see
/// `codex_models_manager::litellm_model_info`), and we do not want a
/// second enrichment pass to second-guess it.
///
/// The no-op contract is pinned by
/// [`tests::enrich_is_noop_for_non_copilot_wires`]. Any future
/// provider that wants to override `populate_model_info` MUST update
/// that test along with the override.
///
/// # Failure handling
///
/// Failures from `populate_model_info` are logged at `warn!` and
/// swallowed: the caller still gets the unenriched `ModelInfo` (the
/// behavior we had before this hook existed), so a transient CAPI
/// hiccup does not block session startup.
pub async fn enrich_model_info(
    provider_info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
    info: &mut ModelInfo,
) {
    let provider = create_model_provider(provider_info, auth_manager);
    if let Err(err) = provider.populate_model_info(info).await {
        tracing::warn!(
            slug = %info.slug,
            error = %err,
            "enrich_model_info: provider.populate_model_info failed; falling back to static catalog metadata"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use codex_login::CodexAuth;
    use codex_model_provider::ModelProvider;
    use codex_model_provider_info::ModelProviderAwsAuthInfo;
    use codex_protocol::config_types::ModelProviderAuthInfo;
    use pretty_assertions::assert_eq;

    use super::*;

    fn provider_info_with_command_auth() -> ModelProviderInfo {
        ModelProviderInfo {
            auth: Some(ModelProviderAuthInfo {
                command: "print-token".to_string(),
                args: Vec::new(),
                timeout_ms: NonZeroU64::new(5_000).expect("timeout should be non-zero"),
                refresh_interval_ms: 300_000,
                cwd: std::env::current_dir()
                    .expect("current dir should be available")
                    .try_into()
                    .expect("current dir should be absolute"),
            }),
            requires_openai_auth: false,
            ..ModelProviderInfo::create_openai_provider(/*base_url*/ None)
        }
    }

    #[test]
    fn command_auth_routes_through_external_bearer_manager() {
        let provider = create_model_provider(
            provider_info_with_command_auth(),
            /*auth_manager*/ None,
        );

        let auth_manager = provider
            .auth_manager()
            .expect("command auth provider should have an auth manager");

        assert!(auth_manager.has_external_auth());
    }

    #[test]
    fn bedrock_wins_over_caller_supplied_openai_manager() {
        let provider = create_model_provider(
            ModelProviderInfo::create_amazon_bedrock_provider(Some(ModelProviderAwsAuthInfo {
                profile: Some("codex-bedrock".to_string()),
                region: None,
            })),
            Some(AuthManager::from_auth_for_testing(CodexAuth::from_api_key(
                "openai-api-key",
            ))),
        );

        assert!(
            provider.auth_manager().is_none(),
            "Bedrock provider must not inherit the caller's OpenAI auth manager"
        );
    }

    #[test]
    fn messages_wire_constructs_anthropic_provider() {
        let info = ModelProviderInfo {
            wire_api: WireApi::Messages,
            ..ModelProviderInfo::create_openai_provider(/*base_url*/ None)
        };
        let provider = create_model_provider(info, /*auth_manager*/ None);

        // The Anthropic provider overrides effective_wire_api to
        // auto-upgrade non-Anthropic slugs; the default impl on
        // ConfiguredModelProvider would keep Messages for any slug.
        // Use that behavioral difference as a witness that we got the
        // right concrete type.
        assert_eq!(
            provider.effective_wire_api("gpt-5.3-codex"),
            WireApi::Responses,
            "Messages-wire provider must auto-upgrade non-Anthropic slugs"
        );
        assert_eq!(
            provider.effective_wire_api("claude-sonnet-4-6"),
            WireApi::Messages,
        );
    }

    #[test]
    fn copilot_wire_constructs_copilot_provider() {
        let info = ModelProviderInfo {
            wire_api: WireApi::Copilot,
            ..ModelProviderInfo::create_openai_provider(/*base_url*/ None)
        };
        let provider = create_model_provider(info, /*auth_manager*/ None);

        // The Copilot provider overrides supports_websockets to false;
        // the default forwards ModelProviderInfo's value (true for
        // OpenAI-style providers). Witness for the concrete type.
        assert!(
            !provider.supports_websockets(),
            "Copilot-wire provider must disable the WS transport"
        );
    }

    #[test]
    fn responses_wire_constructs_configured_provider() {
        let provider = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            /*auth_manager*/ None,
        );

        // ConfiguredModelProvider's default effective_wire_api forwards
        // info.wire_api unchanged — proves we did NOT take the Messages
        // or Copilot arm.
        assert_eq!(provider.effective_wire_api("any-slug"), WireApi::Responses,);
        assert!(provider.extra_request_headers().is_empty());
    }

    /// Pin the no-op contract documented on [`enrich_model_info`].
    ///
    /// Today only Copilot overrides `populate_model_info`. Every other
    /// wire (`llm_proxy_messages`, `responses` / `llm_proxy_responses`,
    /// `generateContent` / Gemini) MUST leave `ModelInfo` untouched so
    /// `enrich_model_info` is a behavioral no-op on those wires —
    /// LiteLLM enrichment for `llm_proxy_*` already happens inside
    /// `models_manager.get_model_info()` and we do not want a second
    /// enrichment pass to second-guess it.
    ///
    /// If a future provider override breaks this contract, update the
    /// crate docs *and* the call-site comments in `core/session/mod.rs`,
    /// `core/session/turn_context.rs`, and `memories/write/runtime.rs`
    /// along with this test — operators expect llm_proxy behavior to
    /// be controlled by the upstream catalog, not by post-hoc Rust
    /// overrides.
    #[tokio::test]
    async fn enrich_is_noop_for_non_copilot_wires() {
        use codex_models_manager::model_info::model_info_from_slug;

        for wire in [
            WireApi::Messages,        // llm_proxy_messages
            WireApi::Responses,       // responses + llm_proxy_responses
            WireApi::GenerateContent, // gemini
        ] {
            let info = ModelProviderInfo {
                wire_api: wire,
                ..ModelProviderInfo::create_openai_provider(None)
            };
            let mut model_info = model_info_from_slug("claude-opus-4.6");
            let before = model_info.clone();
            enrich_model_info(info, /*auth_manager*/ None, &mut model_info).await;
            assert_eq!(
                model_info, before,
                "wire {wire:?} must not mutate ModelInfo — the llm_proxy / \
                 responses / gemini wires rely on models_manager LiteLLM \
                 enrichment being the sole source of truth"
            );
        }
    }

    /// Pin the converse: Copilot MUST mutate `ModelInfo` because the
    /// static catalog returns the Anthropic-direct 1M-context family
    /// default for Claude 4.6+, but CAPI actually caps at 200K. A
    /// regression that turns Copilot's `populate_model_info` back into
    /// a no-op resurrects the 950K-context-window TUI lie.
    #[tokio::test]
    async fn enrich_overrides_claude_context_window_on_copilot_wire() {
        use codex_models_manager::model_info::model_info_from_slug;

        let info = ModelProviderInfo {
            wire_api: WireApi::Copilot,
            ..ModelProviderInfo::create_openai_provider(None)
        };
        let mut model_info = model_info_from_slug("claude-opus-4.6");
        assert_eq!(
            model_info.context_window,
            Some(1_000_000),
            "precondition: static catalog returns the 1M family default"
        );
        enrich_model_info(info, /*auth_manager*/ None, &mut model_info).await;
        assert_eq!(
            model_info.context_window,
            Some(168_000),
            "Copilot enrichment must override the 1M family default with the CAPI 168K prompt cap"
        );
        assert_eq!(model_info.max_context_window, Some(200_000));
    }
}

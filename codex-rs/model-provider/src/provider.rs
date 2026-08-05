use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use codex_api::Provider;
use codex_api::SharedAuthProvider;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::WireApi;
use codex_models_manager::manager::OpenAiModelsManager;
use codex_models_manager::manager::SharedModelsManager;
use codex_models_manager::manager::StaticModelsManager;
use codex_protocol::account::ProviderAccount;
use codex_protocol::error::CodexErr;
use codex_protocol::openai_models::ModelsResponse;
use http::HeaderMap;

use crate::AmazonBedrockModelProvider;
use crate::auth::resolve_provider_auth;
use crate::auth_manager_for_provider;
use crate::models_endpoint::OpenAiModelsEndpoint;
use crate::stream::ProviderResponseStream;
use crate::stream::ProviderStreamRequest;
use codex_protocol::openai_models::ModelInfo;

/// Optional provider-backed features that Codex may expose at runtime.
///
/// These capabilities are a provider-owned upper bound. Callers can disable
/// more functionality through normal config, but should not expose a feature
/// that the active provider marks unsupported here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub namespace_tools: bool,
    pub image_generation: bool,
    pub web_search: bool,
}

impl Default for ProviderCapabilities {
    fn default() -> Self {
        Self {
            namespace_tools: true,
            image_generation: true,
            web_search: true,
        }
    }
}

/// Current app-visible account state for a model provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountState {
    pub account: Option<ProviderAccount>,
    pub requires_openai_auth: bool,
}

/// Error returned when a provider cannot construct its app-visible account state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAccountError {
    MissingChatgptAccountDetails,
}

impl fmt::Display for ProviderAccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingChatgptAccountDetails => {
                write!(
                    f,
                    "email and plan type are required for chatgpt authentication"
                )
            }
        }
    }
}

impl std::error::Error for ProviderAccountError {}

pub type ProviderAccountResult = std::result::Result<ProviderAccountState, ProviderAccountError>;

/// Default model used for automatic approval review when a provider does not
/// require a backend-specific model ID.
pub const DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL: &str = "codex-auto-review";

/// Runtime provider abstraction used by model execution.
///
/// Implementations own provider-specific behavior for a model backend. The
/// `ModelProviderInfo` returned by `info` is the serialized/configured provider
/// metadata used by the default OpenAI-compatible implementation.
///
/// # Extension points (Sortie 1)
///
/// In addition to the auth/info accessors, the trait exposes four small
/// hooks that providers may override to express wire-specific policy without
/// requiring `core/src/client.rs` to branch on `WireApi`:
///
/// * [`effective_wire_api`](Self::effective_wire_api) — pick the actual wire
///   for a given model slug (e.g. auto-upgrade Messages→Responses for non-
///   Anthropic models, or consult a Copilot route table).
/// * [`extra_request_headers`](Self::extra_request_headers) — stamp wire-
///   agnostic headers that the provider's transport always needs.
/// * [`supports_websockets`](Self::supports_websockets) — opt out of the
///   Responses-WebSocket transport for providers whose endpoint is HTTP-SSE
///   only.
/// * [`stream`](Self::stream) — the load-bearing seam that lets the provider
///   own its own wire dispatch so `core` does not have to branch on wire
///   type. The default impl returns `UnsupportedOperation`; the Anthropic
///   Messages and Copilot provider crates override it in Commits 3–4.
///
/// All hooks have default impls that preserve the configured-OpenAI behavior
/// (or, for `stream`, explicitly refuse the call so the legacy in-`core`
/// dispatch path keeps running until Commit 6 collapses it); only providers
/// that need different policy override them.
#[async_trait::async_trait]
pub trait ModelProvider: fmt::Debug + Send + Sync {
    /// Downcast helper for orchestrators that need provider-specific
    /// access not exposed via the trait surface.
    ///
    /// Default impl returns a reference to a unit struct, which
    /// downcasts to `()` — i.e., the orchestrator gets no special
    /// access. Providers that want orchestrator-specific affordances
    /// (e.g. Copilot's lazy CAPI catalog initialization) override
    /// this to return `&self as &dyn Any`.
    ///
    /// Avoid using this on hot paths; it's deliberately ergonomically
    /// awkward to discourage casual coupling. Prefer the typed
    /// extension hooks (`effective_wire_api`, `populate_model_info`,
    /// etc.) when adding new behavior.
    fn as_any(&self) -> &dyn std::any::Any {
        // Default: opaque. Orchestrators that need real access must
        // override this on their concrete provider.
        &()
    }

    /// Returns the configured provider metadata.
    fn info(&self) -> &ModelProviderInfo;

    /// Returns the provider-owned capability upper bounds.
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    /// Returns the preferred model used for automatic approval review.
    ///
    /// Providers that require backend-specific model IDs should override this.
    fn approval_review_preferred_model(&self) -> &'static str {
        DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL
    }

    /// Returns whether requests made through this provider should include attestation.
    fn supports_attestation(&self) -> bool {
        false
    }

    /// Returns the provider-scoped auth manager, when this provider uses one.
    ///
    /// TODO(celia-oai): Make auth manager access internal to this crate so callers
    /// resolve provider-specific auth only through `ModelProvider`. We first need
    /// to think through whether Codex should have a unified provider-specific auth
    /// manager throughout the codebase; that is a larger refactor than this change.
    fn auth_manager(&self) -> Option<Arc<AuthManager>>;

    /// Returns the current provider-scoped auth value, if one is configured.
    async fn auth(&self) -> Option<CodexAuth>;

    /// Returns the current app-visible account state for this provider.
    fn account_state(&self) -> ProviderAccountResult;

    /// Returns provider configuration adapted for the API client.
    async fn api_provider(&self) -> codex_protocol::error::Result<Provider> {
        let auth = self.auth().await;
        self.info()
            .to_api_provider(auth.as_ref().map(CodexAuth::auth_mode))
    }

    /// Returns the provider base URL that will be used at request time.
    async fn runtime_base_url(&self) -> codex_protocol::error::Result<Option<String>> {
        Ok(self.info().base_url.clone())
    }

    /// Returns the auth provider used to attach request credentials.
    async fn api_auth(&self) -> codex_protocol::error::Result<SharedAuthProvider> {
        let auth = self.auth().await;
        resolve_provider_auth(auth.as_ref(), self.info())
    }

    /// Resolves the wire API the provider should use for `model_slug`.
    ///
    /// The default impl returns the statically-configured wire from
    /// [`ModelProviderInfo::wire_api`]. Providers that need slug-aware
    /// routing override this.
    fn effective_wire_api(&self, _model_slug: &str) -> WireApi {
        self.info().wire_api
    }

    /// Provider-scoped HTTP headers stamped on every model request.
    fn extra_request_headers(&self) -> HeaderMap {
        HeaderMap::new()
    }

    /// Whether the provider supports the Responses WebSocket transport.
    fn supports_websockets(&self) -> bool {
        self.info().supports_websockets
    }

    /// Hook for providers to enrich [`ModelInfo`] with backend-specific
    /// per-slug metadata after the static catalog has been applied.
    async fn populate_model_info(
        &self,
        _info: &mut ModelInfo,
    ) -> codex_protocol::error::Result<()> {
        Ok(())
    }

    /// Attempt provider-specific auth recovery after a 401 response.
    async fn try_refresh_auth(&self) -> codex_protocol::error::Result<bool> {
        Ok(false)
    }

    /// Called after a successful stream so the provider can reset any
    /// retry/refresh counters.
    fn note_request_succeeded(&self) {}

    /// Transforms the base URL for Messages-wire requests.
    fn transform_messages_base_url(&self, _url: &mut String) {}

    /// Lifecycle hook called once per session before the first
    /// provider-dispatched turn.
    async fn ensure_session_ctx(&self) -> codex_protocol::error::Result<()> {
        Ok(())
    }

    /// Streams a model response for the given request.
    ///
    /// Default impl returns [`CodexErr::UnsupportedOperation`]; the
    /// Anthropic Messages and Copilot providers override this.
    async fn stream<'a>(
        &'a self,
        _req: ProviderStreamRequest<'a>,
    ) -> codex_protocol::error::Result<ProviderResponseStream> {
        Err(CodexErr::UnsupportedOperation(format!(
            "ModelProvider::stream is not implemented for provider `{name}` \
             (wire={wire:?}); this provider is still dispatched via the legacy \
             in-core path. See sortie-board/xli-v2/sortie-01-modelprovider-extraction.md.",
            name = self.info().name,
            wire = self.info().wire_api,
        )))
    }

    /// Creates the model manager implementation appropriate for this provider.
    fn models_manager(
        &self,
        codex_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager;
}

/// Shared runtime model provider handle.
pub type SharedModelProvider = Arc<dyn ModelProvider>;

/// Creates the default runtime model provider for configured provider metadata.
pub fn create_model_provider(
    provider_info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
) -> SharedModelProvider {
    if provider_info.is_amazon_bedrock() {
        Arc::new(AmazonBedrockModelProvider::new(provider_info))
    } else {
        Arc::new(ConfiguredModelProvider::new(provider_info, auth_manager))
    }
}

/// Runtime model provider backed by configured `ModelProviderInfo`.
///
/// The default implementation for the OpenAI-compatible Responses wire.
/// Constructed directly by `codex-provider-registry::create_model_provider`
/// when `wire_api == WireApi::Responses` and the provider is not
/// Amazon Bedrock. Public + has a `new` constructor so the registry
/// crate can build it without re-running the Bedrock check (the
/// the registry already dispatches on `is_amazon_bedrock` first).
#[derive(Clone, Debug)]
pub struct ConfiguredModelProvider {
    info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
}

impl ConfiguredModelProvider {
    pub fn new(provider_info: ModelProviderInfo, auth_manager: Option<Arc<AuthManager>>) -> Self {
        let auth_manager = auth_manager_for_provider(auth_manager, &provider_info);
        Self {
            info: provider_info,
            auth_manager,
        }
    }
}

#[async_trait::async_trait]
impl ModelProvider for ConfiguredModelProvider {
    fn info(&self) -> &ModelProviderInfo {
        &self.info
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.auth_manager.clone()
    }

    fn supports_attestation(&self) -> bool {
        self.auth_manager
            .as_ref()
            .and_then(|auth_manager| auth_manager.auth_cached())
            .is_some_and(|auth| auth.is_chatgpt_auth())
    }

    async fn auth(&self) -> Option<CodexAuth> {
        match self.auth_manager.as_ref() {
            Some(auth_manager) => auth_manager.auth().await,
            None => None,
        }
    }

    fn account_state(&self) -> ProviderAccountResult {
        let account = if self.info.requires_openai_auth {
            self.auth_manager
                .as_ref()
                .and_then(|auth_manager| {
                    let auth = auth_manager.auth_cached()?;
                    if auth_manager.refresh_failure_for_auth(&auth).is_some() {
                        return None;
                    }
                    Some(auth)
                })
                .map(|auth| match &auth {
                    CodexAuth::ApiKey(_) => Ok(ProviderAccount::ApiKey),
                    CodexAuth::Chatgpt(_)
                    | CodexAuth::ChatgptAuthTokens(_)
                    | CodexAuth::AgentIdentity(_) => {
                        let email = auth.get_account_email();
                        let plan_type = auth.account_plan_type();

                        match (email, plan_type) {
                            (Some(email), Some(plan_type)) => {
                                Ok(ProviderAccount::Chatgpt { email, plan_type })
                            }
                            _ => Err(ProviderAccountError::MissingChatgptAccountDetails),
                        }
                    }
                })
                .transpose()?
        } else {
            None
        };

        Ok(ProviderAccountState {
            account,
            requires_openai_auth: self.info.requires_openai_auth,
        })
    }

    fn models_manager(
        &self,
        codex_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        match config_model_catalog {
            Some(model_catalog) => Arc::new(StaticModelsManager::new(
                self.auth_manager.clone(),
                model_catalog,
            )),
            None => {
                let endpoint = Arc::new(OpenAiModelsEndpoint::new(
                    self.info.clone(),
                    self.auth_manager.clone(),
                ));
                Arc::new(OpenAiModelsManager::new(
                    codex_home,
                    endpoint,
                    self.auth_manager.clone(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use async_trait::async_trait;
    use codex_api::MessagesApiRequest;
    use codex_model_provider_info::ModelProviderAwsAuthInfo;
    use codex_model_provider_info::WireApi;
    use codex_models_manager::manager::RefreshStrategy;
    use codex_protocol::config_types::ModelProviderAuthInfo;
    use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
    use codex_protocol::openai_models::ModelInfo;
    use codex_protocol::openai_models::ModelsResponse;
    use http::HeaderMap;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header_regex;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;
    use crate::stream::MessagesBackend;

    /// Minimal `MessagesBackend` used by the default-refuse trait test.
    /// The default `stream` impl must refuse before ever invoking the
    /// backend — if this ever runs, the test has already failed.
    struct UnreachableBackend;

    #[async_trait]
    impl MessagesBackend for UnreachableBackend {
        async fn execute_messages_turn(
            &self,
            _request: MessagesApiRequest,
            _extra_headers: HeaderMap,
        ) -> codex_protocol::error::Result<codex_prompt::ResponseStream> {
            panic!("default stream impl must refuse before touching the backend");
        }
    }

    fn test_codex_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("codex-model-provider-test-{}", std::process::id()))
    }

    fn provider_for(base_url: String) -> ModelProviderInfo {
        ModelProviderInfo {
            name: "mock".into(),
            base_url: Some(base_url),
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            auth: None,
            aws: None,
            wire_api: WireApi::Responses,
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            request_max_retries: Some(0),
            stream_max_retries: Some(0),
            stream_idle_timeout_ms: Some(5_000),
            websocket_connect_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
        }
    }

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

    fn remote_model(slug: &str) -> ModelInfo {
        serde_json::from_value(json!({
            "slug": slug,
            "display_name": slug,
            "description": null,
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [],
            "shell_type": "shell_command",
            "visibility": "list",
            "supported_in_api": true,
            "priority": 0,
            "upgrade": null,
            "base_instructions": "base instructions",
            "supports_reasoning_summaries": false,
            "support_verbosity": false,
            "default_verbosity": null,
            "apply_patch_tool_type": null,
            "truncation_policy": {"mode": "bytes", "limit": 10_000},
            "supports_parallel_tool_calls": false,
            "supports_image_detail_original": false,
            "context_window": 272_000,
            "max_context_window": 272_000,
            "experimental_supported_tools": [],
        }))
        .expect("valid model")
    }

    fn test_model_info() -> ModelInfo {
        serde_json::from_value(json!({
            "slug": "gpt-test",
            "display_name": "gpt-test",
            "description": "desc",
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [
                {"effort": "medium", "description": "medium"}
            ],
            "shell_type": "shell_command",
            "visibility": "list",
            "supported_in_api": true,
            "priority": 1,
            "upgrade": null,
            "base_instructions": "base instructions",
            "model_messages": null,
            "supports_reasoning_summaries": false,
            "support_verbosity": false,
            "default_verbosity": null,
            "apply_patch_tool_type": null,
            "truncation_policy": {"mode": "bytes", "limit": 10000},
            "supports_parallel_tool_calls": false,
            "supports_image_detail_original": false,
            "context_window": 272000,
            "max_context_window": 272000,
            "experimental_supported_tools": []
        }))
        .expect("test ModelInfo fixture must deserialize")
    }

    /// The three Sortie-1 trait extension hooks must, by default, forward
    /// the underlying `ModelProviderInfo` values.
    #[test]
    fn default_trait_extension_hooks_forward_provider_info() {
        let info = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        let provider = ConfiguredModelProvider::new(info.clone(), /*auth_manager*/ None);
        let seen = provider.info();

        assert_eq!(provider.effective_wire_api("any-model-slug"), seen.wire_api);
        assert_eq!(provider.supports_websockets(), seen.supports_websockets);
        assert!(provider.extra_request_headers().is_empty());
    }

    /// The default `stream` impl must refuse the call with
    /// `UnsupportedOperation`.
    #[tokio::test]
    async fn default_stream_impl_refuses_with_unsupported_operation() {
        let provider = ConfiguredModelProvider::new(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            /*auth_manager*/ None,
        );
        let prompt = codex_prompt::Prompt::default();
        let model_info = test_model_info();
        let backend = UnreachableBackend;
        let request = ProviderStreamRequest {
            prompt: &prompt,
            model_info: &model_info,
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
            backend: &backend,
        };
        let result = provider.stream(request).await;
        let err = match result {
            Ok(_stream) => panic!("default impl must refuse the call, got Ok(ResponseStream)"),
            Err(err) => err,
        };

        match err {
            CodexErr::UnsupportedOperation(msg) => {
                assert!(
                    msg.contains(&provider.info().name),
                    "error message should identify the provider; got: {msg}"
                );
                assert!(
                    msg.contains("ModelProvider::stream"),
                    "error message should name the method; got: {msg}"
                );
            }
            other => panic!("expected UnsupportedOperation, got {other:?}"),
        }
    }

    #[test]
    fn configured_provider_uses_default_capabilities() {
        let provider = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            /*auth_manager*/ None,
        );

        assert_eq!(provider.capabilities(), ProviderCapabilities::default());
    }

    #[test]
    fn configured_provider_uses_default_approval_review_preferred_model() {
        let provider = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.approval_review_preferred_model(),
            DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL
        );
    }

    #[tokio::test]
    async fn configured_provider_runtime_base_url_uses_configured_base_url() {
        let provider = create_model_provider(
            provider_for("https://example.test/v1".to_string()),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider
                .runtime_base_url()
                .await
                .expect("runtime base URL should resolve"),
            Some("https://example.test/v1".to_string())
        );
    }

    #[test]
    fn create_model_provider_builds_command_auth_manager_without_base_manager() {
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
    fn openai_provider_returns_unauthenticated_openai_account_state() {
        let provider = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.account_state(),
            Ok(ProviderAccountState {
                account: None,
                requires_openai_auth: true,
            })
        );
    }

    #[test]
    fn openai_provider_returns_api_key_account_state() {
        let provider = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            Some(AuthManager::from_auth_for_testing(CodexAuth::from_api_key(
                "openai-api-key",
            ))),
        );

        assert_eq!(
            provider.account_state(),
            Ok(ProviderAccountState {
                account: Some(ProviderAccount::ApiKey),
                requires_openai_auth: true,
            })
        );
    }

    #[test]
    fn custom_non_openai_provider_returns_no_account_state() {
        let provider = create_model_provider(
            ModelProviderInfo {
                name: "Custom".to_string(),
                base_url: Some("http://localhost:1234/v1".to_string()),
                wire_api: WireApi::Responses,
                requires_openai_auth: false,
                ..Default::default()
            },
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.account_state(),
            Ok(ProviderAccountState {
                account: None,
                requires_openai_auth: false,
            })
        );
    }

    #[test]
    fn amazon_bedrock_provider_returns_bedrock_account_state() {
        let provider = create_model_provider(
            ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.account_state(),
            Ok(ProviderAccountState {
                account: Some(ProviderAccount::AmazonBedrock),
                requires_openai_auth: false,
            })
        );
    }

    #[tokio::test]
    async fn amazon_bedrock_provider_creates_static_models_manager() {
        let provider = create_model_provider(
            ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
            /*auth_manager*/ None,
        );
        let manager =
            provider.models_manager(test_codex_home(), /*config_model_catalog*/ None);

        let catalog = manager.raw_model_catalog(RefreshStrategy::Online).await;
        let model_ids = catalog
            .models
            .iter()
            .map(|model| model.slug.as_str())
            .collect::<Vec<_>>();

        assert_eq!(model_ids, vec!["openai.gpt-5.5", "openai.gpt-5.4"]);

        let default_model = manager
            .list_models(RefreshStrategy::Online)
            .await
            .into_iter()
            .find(|preset| preset.is_default)
            .expect("Bedrock catalog should have a default model");

        assert_eq!(default_model.model, "openai.gpt-5.5");
    }

    #[tokio::test]
    async fn amazon_bedrock_provider_uses_configured_static_catalog_when_present() {
        let custom_model =
            codex_models_manager::model_info::model_info_from_slug("custom-bedrock-model");

        let provider = create_model_provider(
            ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
            /*auth_manager*/ None,
        );
        let manager = provider.models_manager(
            test_codex_home(),
            Some(ModelsResponse {
                models: vec![custom_model],
            }),
        );

        let catalog = manager.raw_model_catalog(RefreshStrategy::Online).await;

        assert_eq!(catalog.models.len(), 1);
        assert_eq!(catalog.models[0].slug, "custom-bedrock-model");
    }

    #[tokio::test]
    async fn configured_provider_models_manager_uses_provider_bearer_token() {
        let server = MockServer::start().await;
        let remote_models = vec![remote_model("provider-model")];

        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header_regex("Authorization", "Bearer provider-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(ModelsResponse {
                        models: remote_models.clone(),
                    }),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut provider_info = provider_for(server.uri());
        provider_info.experimental_bearer_token = Some("provider-token".to_string());
        let provider = create_model_provider(
            provider_info,
            Some(AuthManager::from_auth_for_testing(
                CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            )),
        );

        let manager =
            provider.models_manager(test_codex_home(), /*config_model_catalog*/ None);
        let catalog = manager.raw_model_catalog(RefreshStrategy::Online).await;

        assert!(
            catalog
                .models
                .iter()
                .any(|model| model.slug == "provider-model")
        );
    }
}

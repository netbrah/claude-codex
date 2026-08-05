//! LiteLLM `/v1/model/info` live catalog provider.
//!
//! Fetches model metadata from LiteLLM-compatible proxy endpoints and
//! enriches the fallback `ModelInfo` for models
//! not found in the bundled OpenAI catalog (e.g. Claude models).
//!
//! The endpoint returns per-model capabilities including context window
//! sizes, supported features (vision, reasoning, prompt caching, assistant
//! prefill), and cost information. This eliminates the hardcoded fallback
//! that incorrectly gives Claude models a 272K context window instead of
//! their actual 200K-1M window.
//!
//! On any failure (network, auth, parse) this returns `None` and callers
//! fall back to the static `model_info_from_slug` defaults. The feature
//! degrades gracefully -- it never blocks session startup.

use serde::Deserialize;
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::debug;
use tracing::warn;

use crate::provider_caps::ProviderCaps;
use crate::provider_caps::ProviderCapsPatch;

/// Subset of LiteLLM model_info fields we consume.
#[derive(Debug, Clone)]
pub(crate) struct LiteLLMModelCaps {
    pub slug: String,
    pub max_input_tokens: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub supports_vision: Option<bool>,
    pub supports_function_calling: Option<bool>,
    pub supports_assistant_prefill: Option<bool>,
    pub supports_prompt_caching: Option<bool>,
}

/// Cached live catalog, keyed by model slug.
#[derive(Debug, Default)]
pub(crate) struct LiteLLMCatalog {
    models: RwLock<HashMap<String, LiteLLMModelCaps>>,
}

impl LiteLLMCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a model slug in the cached catalog.
    pub async fn get(&self, slug: &str) -> Option<LiteLLMModelCaps> {
        let models = self.models.read().await;
        models.get(slug).cloned()
    }

    /// Replace the catalog contents after a successful fetch.
    pub async fn replace(&self, entries: HashMap<String, LiteLLMModelCaps>) {
        let mut models = self.models.write().await;
        *models = entries;
    }

    /// Whether the catalog has been populated.
    pub async fn is_empty(&self) -> bool {
        let models = self.models.read().await;
        models.is_empty()
    }
}

/// Raw JSON shapes from the LiteLLM `/v1/model/info` endpoint.
#[derive(Deserialize)]
struct LiteLLMResponse {
    data: Vec<LiteLLMEntry>,
}

#[derive(Deserialize)]
struct LiteLLMEntry {
    model_name: Option<String>,
    #[serde(default)]
    model_info: LiteLLMModelInfoRaw,
}

#[derive(Deserialize, Default)]
struct LiteLLMModelInfoRaw {
    max_input_tokens: Option<i64>,
    max_output_tokens: Option<i64>,
    #[serde(default)]
    supports_vision: Option<bool>,
    #[serde(default)]
    supports_function_calling: Option<bool>,
    #[serde(default)]
    supports_assistant_prefill: Option<bool>,
    #[serde(default)]
    supports_prompt_caching: Option<bool>,
}

/// Fetch the LiteLLM model catalog from the provider's base URL.
///
/// Returns a slug-keyed map on success, `None` on any failure. The caller
/// should populate the `LiteLLMCatalog` cache with the result.
///
/// Timeout is 5 seconds -- the endpoint returns quickly but we don't want
/// to block session startup on a slow proxy.
pub(crate) async fn fetch_litellm_model_info(
    base_url: &str,
    api_key: &str,
) -> Option<HashMap<String, LiteLLMModelCaps>> {
    if base_url.is_empty() || api_key.is_empty() {
        return None;
    }

    // Strip /v1 suffix if present -- the endpoint path is /v1/model/info
    // and base_url may already include /v1.
    let base = base_url.trim_end_matches('/');
    let url = if base.ends_with("/v1") {
        format!("{base}/model/info")
    } else {
        format!("{base}/v1/model/info")
    };

    debug!("fetching LiteLLM model info from {url}");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;

    let resp = client
        .get(&url)
        .header("accept", "application/json")
        .header("x-api-key", api_key)
        .header("authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        warn!(
            status = %resp.status(),
            "LiteLLM /v1/model/info returned non-success"
        );
        return None;
    }

    let body: LiteLLMResponse = resp.json().await.ok()?;

    let mut map = HashMap::new();
    for entry in body.data {
        let Some(slug) = entry.model_name else {
            continue;
        };
        if slug.is_empty() {
            continue;
        }
        let mi = entry.model_info;
        map.insert(
            slug.clone(),
            LiteLLMModelCaps {
                slug,
                max_input_tokens: mi.max_input_tokens,
                max_output_tokens: mi.max_output_tokens,
                supports_vision: mi.supports_vision,
                supports_function_calling: mi.supports_function_calling,
                supports_assistant_prefill: mi.supports_assistant_prefill,
                supports_prompt_caching: mi.supports_prompt_caching,
            },
        );
    }

    if map.is_empty() {
        warn!("LiteLLM /v1/model/info returned empty model list");
        return None;
    }

    debug!(count = map.len(), "LiteLLM model info fetched successfully");
    Some(map)
}

/// Enrich a fallback `ModelInfo` with live LiteLLM caps.
///
/// Overwrites context_window, max_context_window, input_modalities, and
/// supports_parallel_tool_calls based on the live catalog data.
pub(crate) fn enrich_model_info_from_litellm(
    model: &mut codex_protocol::openai_models::ModelInfo,
    caps: &LiteLLMModelCaps,
) {
    use codex_protocol::openai_models::InputModality;

    if let Some(max_input) = caps.max_input_tokens {
        model.context_window = Some(max_input);
        model.max_context_window = Some(max_input);
    }

    if caps.supports_vision == Some(true) {
        if !model.input_modalities.contains(&InputModality::Image) {
            model.input_modalities.push(InputModality::Image);
        }
    }

    if caps.supports_function_calling == Some(true) {
        model.supports_parallel_tool_calls = true;
    }

    if let Some(max_output) = caps.max_output_tokens {
        ProviderCaps::merge_override(
            &model.slug,
            ProviderCapsPatch {
                max_output_tokens: Some(Some(max_output)),
                ..Default::default()
            },
        );
    }

    if let Some(caching) = caps.supports_prompt_caching {
        ProviderCaps::merge_override(
            &model.slug,
            ProviderCapsPatch {
                supports_prompt_caching: Some(caching),
                ..Default::default()
            },
        );
    }

    if let Some(prefill) = caps.supports_assistant_prefill {
        ProviderCaps::merge_override(
            &model.slug,
            ProviderCapsPatch {
                supports_assistant_prefill: Some(prefill),
                ..Default::default()
            },
        );
    }

    // Mark as no longer pure-fallback since we have live data.
    model.used_fallback_model_metadata = false;

    debug!(
        slug = %model.slug,
        context_window = ?model.context_window,
        parallel_tool_calls = model.supports_parallel_tool_calls,
        "enriched model info from LiteLLM catalog"
    );
}

/// Public crate API: fetch the LiteLLM `/v1/model/info` catalog and enrich
/// every entry in `models` whose slug matches a catalog entry.
///
/// Wraps [`fetch_litellm_model_info`] + [`enrich_model_info_from_litellm`]
/// so callers in other crates (e.g. `codex-model-provider`) don't need to
/// reach into the internal types. On any failure (network, auth, parse)
/// the function returns silently with `models` left unchanged — the
/// `warn!` logs inside `fetch_litellm_model_info` carry the diagnostic.
pub async fn enrich_models_with_litellm(
    models: &mut [codex_protocol::openai_models::ModelInfo],
    base_url: &str,
    api_key: &str,
) {
    if api_key.is_empty() {
        return;
    }
    let Some(catalog) = fetch_litellm_model_info(base_url, api_key).await else {
        return;
    };
    for model in models.iter_mut() {
        if let Some(caps) = catalog.get(&model.slug) {
            enrich_model_info_from_litellm(model, caps);
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parse_litellm_response() {
        let json = serde_json::json!({
            "data": [
                {
                    "model_name": "claude-opus-4.6",
                    "model_info": {
                        "max_input_tokens": 1000000,
                        "max_output_tokens": 128000,
                        "supports_vision": true,
                        "supports_function_calling": true,
                        "supports_assistant_prefill": false,
                        "supports_prompt_caching": true
                    }
                },
                {
                    "model_name": "claude-haiku-4.5",
                    "model_info": {
                        "max_input_tokens": 200000,
                        "max_output_tokens": 8192,
                        "supports_vision": true,
                        "supports_function_calling": true,
                        "supports_assistant_prefill": true,
                        "supports_prompt_caching": true
                    }
                }
            ]
        });

        let resp: LiteLLMResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.data.len(), 2);
        assert_eq!(resp.data[0].model_info.max_input_tokens, Some(1_000_000));
        assert_eq!(resp.data[1].model_info.max_input_tokens, Some(200_000));
    }

    #[test]
    fn enrich_sets_context_window_and_parallel_tools() {
        let mut model = crate::model_info::model_info_from_slug("claude-opus-4.6");
        assert_eq!(model.context_window, Some(1_000_000));
        assert!(model.supports_parallel_tool_calls);
        assert!(!model.used_fallback_model_metadata);

        let caps = LiteLLMModelCaps {
            slug: "claude-opus-4.6".to_string(),
            max_input_tokens: Some(1_000_000),
            max_output_tokens: Some(128_000),
            supports_vision: Some(true),
            supports_function_calling: Some(true),
            supports_assistant_prefill: Some(false),
            supports_prompt_caching: Some(true),
        };

        enrich_model_info_from_litellm(&mut model, &caps);

        assert_eq!(model.context_window, Some(1_000_000));
        assert_eq!(model.max_context_window, Some(1_000_000));
        assert!(model.supports_parallel_tool_calls);
        assert!(!model.used_fallback_model_metadata);
    }

    #[test]
    fn enrich_is_noop_when_caps_have_no_data() {
        let mut model = crate::model_info::model_info_from_slug("unknown-model");
        let original_window = model.context_window;

        let caps = LiteLLMModelCaps {
            slug: "unknown-model".to_string(),
            max_input_tokens: None,
            max_output_tokens: None,
            supports_vision: None,
            supports_function_calling: None,
            supports_assistant_prefill: None,
            supports_prompt_caching: None,
        };

        enrich_model_info_from_litellm(&mut model, &caps);
        assert_eq!(model.context_window, original_window);
    }
}


//! CAPI `/models` catalog — per-slug capabilities discovered at session start.
//!
//! GitHub Copilot's enterprise API exposes a `/models` endpoint that returns
//! a per-slug capabilities envelope. Per-slug context windows, prompt-token
//! caps, output-token caps, and the allowed values for `output_config.effort`
//! / `reasoning.effort` all live here.
//!
//! Without this catalog, [`CopilotModelProvider`] falls back on the family-
//! default heuristics in `codex-models-manager`, which assume Anthropic's
//! 1M-context beta SKU on Claude 4.6+ and lead to a misleading TUI display
//! (the "950K context window" lie) and silent surprise truncations on long
//! Copilot sessions. They also let pre-flight effort validation fail
//! gracelessly with a CAPI 400.
//!
//! This module owns:
//!
//! 1. Pure deserialize types (`CapiModelInfo`, `CapiModelLimits`,
//!    `CapiModelSupports`) covering the subset of the response we consume.
//!    Forward-compat: unknown fields ignored.
//! 2. The lazy HTTP fetcher (`fetch_capi_models`) that POSTs against
//!    `{endpoints.api}/models` with the live CAPI JWT.
//!
//! Caching lives on [`crate::CopilotCtx`] (`OnceCell` keyed per session).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

/// Verbatim subset of the CAPI per-model `limits` envelope.
///
/// CAPI returns more fields (`vision`, etc.) but we only consume the token
/// budgets. Other fields are ignored via `serde(default)` permissiveness.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CapiModelLimits {
    /// Total (prompt + output) ceiling. Maps to `ModelInfo::max_context_window`.
    #[serde(default)]
    pub max_context_window_tokens: Option<i64>,
    /// Prompt-only ceiling. Lower than `max_context_window_tokens` because
    /// CAPI reserves headroom for output. Maps to `ModelInfo::context_window`.
    #[serde(default)]
    pub max_prompt_tokens: Option<i64>,
    /// Streaming output cap.
    #[serde(default)]
    pub max_output_tokens: Option<i64>,
    /// Non-streaming output cap (typically lower than `max_output_tokens`).
    #[serde(default)]
    pub max_non_streaming_output_tokens: Option<i64>,
}

/// Verbatim subset of the CAPI per-model `supports` envelope.
///
/// `reasoning_effort` is the load-bearing field for pre-flight validation:
/// CAPI rejects requests where `output_config.effort` (Anthropic route) or
/// `reasoning.effort` (Responses route) isn't in this list.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CapiModelSupports {
    /// Allowed values for the effort dial. Empty = no validation possible.
    /// Examples (verified 2026-05-28):
    /// - `claude-opus-4.7` -> `["medium"]`
    /// - `claude-opus-4.8` -> `["medium"]`  (same server-side lock as 4.7)
    /// - `claude-opus-4.6` -> `["low", "medium", "high"]`
    /// - `gpt-5.4`         -> `["low", "medium", "high", "xhigh"]`
    /// - `gpt-5.4-mini`    -> `["none", "low", "medium", "high", "xhigh"]`
    #[serde(default)]
    pub reasoning_effort: Vec<String>,
    /// Whether the model accepts adaptive thinking. All Claude 4.6+ slugs
    /// on Copilot return `true`; not consumed by validation today.
    #[serde(default)]
    pub adaptive_thinking: Option<bool>,
    /// Maximum thinking budget in tokens. Informational only; XLI sends
    /// `thinking: {type: adaptive}` which lets the model self-regulate.
    #[serde(default)]
    pub max_thinking_budget: Option<i64>,
    #[serde(default)]
    pub min_thinking_budget: Option<i64>,
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default)]
    pub streaming: Option<bool>,
    #[serde(default)]
    pub structured_outputs: Option<bool>,
    #[serde(default)]
    pub tool_calls: Option<bool>,
}

/// One model entry from CAPI's `/models` response.
///
/// Public API shape: `limits` and `supports` are exposed at the top
/// level for caller ergonomics. CAPI's wire shape nests them inside
/// `capabilities`; the `Deserialize` impl below flattens the wire
/// shape into this struct on parse.
#[derive(Debug, Clone)]
pub struct CapiModelInfo {
    pub id: String,
    pub name: Option<String>,
    pub vendor: Option<String>,
    pub limits: CapiModelLimits,
    pub supports: CapiModelSupports,
}

/// Wire-shape mirror of CAPI's `/models` entry. CAPI nests `limits` and
/// `supports` under `capabilities`; this struct deserializes that shape
/// verbatim, then `From` flattens it for the public `CapiModelInfo`.
#[derive(Debug, Clone, Deserialize)]
struct CapiModelInfoWire {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    capabilities: CapiCapabilitiesWire,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CapiCapabilitiesWire {
    #[serde(default)]
    limits: CapiModelLimits,
    #[serde(default)]
    supports: CapiModelSupports,
}

impl From<CapiModelInfoWire> for CapiModelInfo {
    fn from(w: CapiModelInfoWire) -> Self {
        Self {
            id: w.id,
            name: w.name,
            vendor: w.vendor,
            limits: w.capabilities.limits,
            supports: w.capabilities.supports,
        }
    }
}

/// Parsed CAPI catalog, keyed by slug.
///
/// Wrapped in `Arc` and shared via `CopilotCtx::models()`. Constructed
/// once per session; never mutated thereafter.
pub type CapiModelCatalog = Arc<HashMap<String, CapiModelInfo>>;

#[derive(Deserialize)]
struct CapiModelsResponse {
    #[serde(default)]
    data: Vec<CapiModelInfoWire>,
}

/// Fetch CAPI's `/models` envelope for the current session.
///
/// `base_url` is the live `endpoints.api` from the CAPI auth snapshot
/// (typically `https://api.enterprise.githubcopilot.com`). `bearer` is
/// the freshly-minted CAPI JWT — the same one used for inference calls.
///
/// On any failure (network, auth, parse) returns an empty map. The caller
/// — `CopilotCtx::models()` — caches the empty map so we don't hammer
/// CAPI on subsequent calls; provider fallback paths take over and the
/// session keeps running. Failure is non-fatal because XLI worked for
/// months without this endpoint and the family-default heuristics are
/// adequate when CAPI is unreachable.
///
/// Timeout: 5 seconds. CAPI typically responds in <500ms; we don't want
/// to block session startup on a slow proxy or transient CAPI hiccup.
pub async fn fetch_capi_models(
    http: &reqwest::Client,
    bearer: &str,
    base_url: &str,
    extra_headers: &http::HeaderMap,
) -> CapiModelCatalog {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut builder = http
        .get(&url)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(5));

    for (name, value) in extra_headers {
        builder = builder.header(name.as_str(), value);
    }

    let resp = match builder.send().await {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(
                target: "copilot::capi_models",
                error = %err,
                "fetch_capi_models: HTTP error; falling back to family defaults"
            );
            return Arc::new(HashMap::new());
        }
    };

    if !resp.status().is_success() {
        tracing::warn!(
            target: "copilot::capi_models",
            status = %resp.status(),
            "fetch_capi_models: non-success status; falling back to family defaults"
        );
        return Arc::new(HashMap::new());
    }

    let body: CapiModelsResponse = match resp.json().await {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(
                target: "copilot::capi_models",
                error = %err,
                "fetch_capi_models: parse error; falling back to family defaults"
            );
            return Arc::new(HashMap::new());
        }
    };

    let mut map = HashMap::with_capacity(body.data.len());
    for wire in body.data {
        let info: CapiModelInfo = wire.into();
        map.insert(info.id.clone(), info);
    }

    tracing::debug!(
        target: "copilot::capi_models",
        count = map.len(),
        "fetch_capi_models: catalog populated"
    );

    Arc::new(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_capi_models_envelope() {
        // CAPI's actual wire shape nests limits + supports under
        // `capabilities`. The wire-shape struct + From impl flatten
        // that into the public `CapiModelInfo`.
        let raw = serde_json::json!({
            "data": [
                {
                    "id": "claude-opus-4.7",
                    "name": "Claude Opus 4.7",
                    "vendor": "Anthropic",
                    "capabilities": {
                        "family": "claude-opus-4.7",
                        "limits": {
                            "max_context_window_tokens": 200000,
                            "max_prompt_tokens": 168000,
                            "max_output_tokens": 32000,
                            "max_non_streaming_output_tokens": 16000
                        },
                        "supports": {
                            "reasoning_effort": ["medium"],
                            "adaptive_thinking": true,
                            "max_thinking_budget": 32000,
                            "min_thinking_budget": 1024,
                            "parallel_tool_calls": true,
                            "streaming": true,
                            "structured_outputs": true,
                            "tool_calls": true
                        }
                    }
                },
                {
                    "id": "gpt-5.4",
                    "vendor": "OpenAI",
                    "capabilities": {
                        "limits": {
                            "max_context_window_tokens": 400000,
                            "max_prompt_tokens": 272000,
                            "max_output_tokens": 128000
                        },
                        "supports": {
                            "reasoning_effort": ["low", "medium", "high", "xhigh"]
                        }
                    }
                }
            ]
        });

        let parsed: CapiModelsResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.data.len(), 2);

        // Convert to public shape and verify the flatten.
        let opus: CapiModelInfo = parsed.data[0].clone().into();
        assert_eq!(opus.id, "claude-opus-4.7");
        assert_eq!(opus.limits.max_context_window_tokens, Some(200_000));
        assert_eq!(opus.limits.max_prompt_tokens, Some(168_000));
        assert_eq!(opus.supports.reasoning_effort, vec!["medium".to_string()]);

        let gpt: CapiModelInfo = parsed.data[1].clone().into();
        assert_eq!(gpt.limits.max_context_window_tokens, Some(400_000));
        assert_eq!(
            gpt.supports.reasoning_effort,
            vec![
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
                "xhigh".to_string()
            ]
        );
    }

    #[test]
    fn deserialize_tolerates_missing_optional_fields() {
        // No `capabilities` block at all — wire-shape `capabilities`
        // defaults to empty, which flattens into empty limits/supports.
        let raw = serde_json::json!({
            "data": [{ "id": "minimal" }]
        });
        let parsed: CapiModelsResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.data.len(), 1);
        let info: CapiModelInfo = parsed.data[0].clone().into();
        assert_eq!(info.id, "minimal");
        assert!(info.supports.reasoning_effort.is_empty());
        assert!(info.limits.max_context_window_tokens.is_none());
    }

    #[test]
    fn deserialize_tolerates_unknown_fields() {
        // CAPI may add fields in the future — we must not break.
        let raw = serde_json::json!({
            "data": [{
                "id": "future-model",
                "future_field_we_dont_know": {"nested": [1, 2, 3]},
                "capabilities": {
                    "future_capability_block": {"nested": true},
                    "supports": {
                        "reasoning_effort": ["medium"],
                        "future_capability": true
                    }
                }
            }]
        });
        let parsed: CapiModelsResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.data.len(), 1);
        let info: CapiModelInfo = parsed.data[0].clone().into();
        assert_eq!(info.supports.reasoning_effort, vec!["medium"]);
    }
}

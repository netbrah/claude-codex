//! Typed wire vocabulary for the Google Gemini `streamGenerateContent`
//! protocol.
//!
//! Rung-3 of the harness-invariant ladder (S-WIRE-VOCAB-MAX-TEETH), Gemini
//! half. Mirrors the Anthropic `messages_wire_types.rs` module.
//!
//! Unlike Anthropic (which tags every SSE event with a `type` field), a
//! Gemini `Part` is a *union by presence of key* — `{"text": "..."}` vs
//! `{"functionCall": {...}}` vs `{"inlineData": {...}}`. A `Part` may also
//! carry orthogonal modifiers (`thought: bool`, `thoughtSignature: String`)
//! alongside its payload. We model that as a `Part { payload, thought,
//! thought_signature }` struct whose `payload` is the typed [`PartPayload`]
//! enum.
//!
//! Both [`PartPayload`] and [`FinishReason`] carry an `Unknown` arm so a
//! never-before-seen part key or finish reason routes to a named,
//! inspectable variant with a `tracing::warn!` rather than a silent serde
//! drop or a stringly-typed `_ =>` arm in the parser.
//!
//! Source of truth for the wire vocabulary:
//!   https://ai.google.dev/api/rest/v1beta/Content
//!   https://ai.google.dev/api/rest/v1beta/GenerateContentResponse

use serde::Deserialize;
use serde::Deserializer;

/// One `candidates[].content.parts[]` element.
///
/// `payload` is the typed union member; `thought` / `thought_signature`
/// are orthogonal modifiers that may decorate any payload (in practice a
/// thinking text part or a signed function call).
#[derive(Debug)]
pub(crate) struct Part {
    pub(crate) payload: PartPayload,
    pub(crate) thought: bool,
    pub(crate) thought_signature: Option<String>,
}

/// The payload union inside a Gemini `Part`.
#[derive(Debug)]
pub(crate) enum PartPayload {
    Text(String),
    FunctionCall(GeminiFunctionCall),
    /// `functionResponse` — a tool result echoed by the model. XLI never
    /// asks Gemini to emit these inbound, but we name it rather than drop.
    FunctionResponse(#[allow(dead_code)] serde_json::Value),
    /// `inlineData` — base64-embedded media. Not surfaced in XLI yet.
    InlineData(#[allow(dead_code)] serde_json::Value),
    /// `fileData` — file-uri media. Not surfaced in XLI yet.
    FileData(#[allow(dead_code)] serde_json::Value),
    /// `executableCode` — model-generated code for server-side execution.
    ExecutableCode(#[allow(dead_code)] serde_json::Value),
    /// `codeExecutionResult` — server-side code-execution tool result.
    CodeExecutionResult(#[allow(dead_code)] serde_json::Value),
    /// A part object carrying only modifiers (e.g. a bare `thoughtSignature`)
    /// with no recognised payload key, or an empty `{}`.
    Empty,
    /// A part key not in our known vocabulary. Carries the offending key and
    /// the raw JSON so the parser can `tracing::warn!` with full fidelity.
    Unknown {
        tag: String,
        #[allow(dead_code)]
        raw: serde_json::Value,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiFunctionCall {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) args: Option<serde_json::Value>,
}

/// Known modifier keys that decorate a payload rather than being one.
const MODIFIER_KEYS: &[&str] = &["thought", "thoughtSignature"];

/// Known payload keys, in dispatch priority order. `text` wins first
/// because thinking parts carry `text` + `thought: true`.
const PAYLOAD_KEYS: &[&str] = &[
    "text",
    "functionCall",
    "functionResponse",
    "inlineData",
    "fileData",
    "executableCode",
    "codeExecutionResult",
];

impl<'de> Deserialize<'de> for Part {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let obj = match raw.as_object() {
            Some(o) => o,
            None => {
                tracing::warn!("gemini Part was not a JSON object; routing payload to Unknown",);
                return Ok(Part {
                    payload: PartPayload::Unknown {
                        tag: "<non-object>".to_owned(),
                        raw,
                    },
                    thought: false,
                    thought_signature: None,
                });
            }
        };

        let thought = obj
            .get("thought")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let thought_signature = obj
            .get("thoughtSignature")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);

        // Dispatch on the first recognised payload key.
        let payload = if let Some(text) = obj.get("text") {
            // `text` may legitimately be a JSON null in malformed payloads.
            PartPayload::Text(text.as_str().unwrap_or_default().to_owned())
        } else if let Some(fc) = obj.get("functionCall") {
            match serde_json::from_value::<GeminiFunctionCall>(fc.clone()) {
                Ok(parsed) => PartPayload::FunctionCall(parsed),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "gemini functionCall part with malformed payload; routing to Unknown",
                    );
                    PartPayload::Unknown {
                        tag: "functionCall".to_owned(),
                        raw,
                    }
                }
            }
        } else if let Some(v) = obj.get("functionResponse") {
            PartPayload::FunctionResponse(v.clone())
        } else if let Some(v) = obj.get("inlineData") {
            PartPayload::InlineData(v.clone())
        } else if let Some(v) = obj.get("fileData") {
            PartPayload::FileData(v.clone())
        } else if let Some(v) = obj.get("executableCode") {
            PartPayload::ExecutableCode(v.clone())
        } else if let Some(v) = obj.get("codeExecutionResult") {
            PartPayload::CodeExecutionResult(v.clone())
        } else {
            // No known payload key. Distinguish "modifiers only / empty"
            // from "genuinely unknown key" so a new upstream part type is
            // surfaced rather than silently treated as Empty.
            let unknown_key = obj.keys().find(|k| {
                !MODIFIER_KEYS.contains(&k.as_str()) && !PAYLOAD_KEYS.contains(&k.as_str())
            });
            match unknown_key {
                Some(key) => {
                    tracing::warn!(
                        part_key = %key,
                        "unknown gemini Part key; routing payload to Unknown",
                    );
                    PartPayload::Unknown {
                        tag: key.clone(),
                        raw,
                    }
                }
                None => PartPayload::Empty,
            }
        };

        Ok(Part {
            payload,
            thought,
            thought_signature,
        })
    }
}

/// A Gemini candidate `finishReason`.
///
/// Variants map 1:1 to the SCREAMING_SNAKE_CASE wire strings documented in
/// `GenerateContentResponse.Candidate.FinishReason`. An unrecognised reason
/// is preserved verbatim in [`FinishReason::Unknown`] rather than collapsing
/// into a `_ =>` arm — so a new upstream terminal state is named, logged,
/// and round-trippable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FinishReason {
    Stop,
    MaxTokens,
    Safety,
    Recitation,
    Language,
    Other,
    Blocklist,
    ProhibitedContent,
    Spii,
    MalformedFunctionCall,
    ImageSafety,
    /// Finish reason not in our known vocabulary; raw string preserved.
    Unknown(String),
}

impl FinishReason {
    pub(crate) fn from_wire(s: &str) -> Self {
        match s {
            "STOP" => FinishReason::Stop,
            "MAX_TOKENS" => FinishReason::MaxTokens,
            "SAFETY" => FinishReason::Safety,
            "RECITATION" => FinishReason::Recitation,
            "LANGUAGE" => FinishReason::Language,
            "OTHER" => FinishReason::Other,
            "BLOCKLIST" => FinishReason::Blocklist,
            "PROHIBITED_CONTENT" => FinishReason::ProhibitedContent,
            "SPII" => FinishReason::Spii,
            "MALFORMED_FUNCTION_CALL" => FinishReason::MalformedFunctionCall,
            "IMAGE_SAFETY" => FinishReason::ImageSafety,
            other => {
                tracing::warn!(
                    finish_reason = %other,
                    "unknown gemini finishReason; routing to FinishReason::Unknown",
                );
                FinishReason::Unknown(other.to_owned())
            }
        }
    }

    /// Canonical wire string for this reason (verbatim for `Unknown`).
    pub(crate) fn as_wire_str(&self) -> &str {
        match self {
            FinishReason::Stop => "STOP",
            FinishReason::MaxTokens => "MAX_TOKENS",
            FinishReason::Safety => "SAFETY",
            FinishReason::Recitation => "RECITATION",
            FinishReason::Language => "LANGUAGE",
            FinishReason::Other => "OTHER",
            FinishReason::Blocklist => "BLOCKLIST",
            FinishReason::ProhibitedContent => "PROHIBITED_CONTENT",
            FinishReason::Spii => "SPII",
            FinishReason::MalformedFunctionCall => "MALFORMED_FUNCTION_CALL",
            FinishReason::ImageSafety => "IMAGE_SAFETY",
            FinishReason::Unknown(s) => s.as_str(),
        }
    }
}

impl<'de> Deserialize<'de> for FinishReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(FinishReason::from_wire(&s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(json: &str) -> Part {
        serde_json::from_str(json).expect("part should deserialize")
    }

    #[test]
    fn text_part_parses() {
        let p = part(r#"{"text":"hello"}"#);
        assert!(!p.thought);
        assert!(p.thought_signature.is_none());
        match p.payload {
            PartPayload::Text(t) => assert_eq!(t, "hello"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn thinking_text_part_sets_modifier() {
        let p = part(r#"{"text":"reasoning","thought":true}"#);
        assert!(p.thought);
        assert!(matches!(p.payload, PartPayload::Text(_)));
    }

    #[test]
    fn function_call_part_parses() {
        let p = part(r#"{"functionCall":{"name":"shell","args":{"command":["ls"]}}}"#);
        match p.payload {
            PartPayload::FunctionCall(fc) => {
                assert_eq!(fc.name, "shell");
                assert!(fc.args.is_some());
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn function_call_with_signature_keeps_modifier() {
        let p = part(r#"{"functionCall":{"name":"shell","args":{}},"thoughtSignature":"SIG_ABC"}"#);
        assert_eq!(p.thought_signature.as_deref(), Some("SIG_ABC"));
        assert!(matches!(p.payload, PartPayload::FunctionCall(_)));
    }

    #[test]
    fn inline_data_part_is_named_not_dropped() {
        let p = part(r#"{"inlineData":{"mimeType":"image/png","data":"AAAA"}}"#);
        assert!(matches!(p.payload, PartPayload::InlineData(_)));
    }

    #[test]
    fn code_execution_result_part_is_named() {
        let p = part(r#"{"codeExecutionResult":{"outcome":"OK","output":"42"}}"#);
        assert!(matches!(p.payload, PartPayload::CodeExecutionResult(_)));
    }

    #[test]
    fn signature_only_part_is_empty_payload() {
        let p = part(r#"{"thoughtSignature":"SIG_ONLY"}"#);
        assert_eq!(p.thought_signature.as_deref(), Some("SIG_ONLY"));
        assert!(matches!(p.payload, PartPayload::Empty));
    }

    #[test]
    fn empty_object_part_is_empty_payload() {
        let p = part(r#"{}"#);
        assert!(matches!(p.payload, PartPayload::Empty));
    }

    #[test]
    fn unknown_part_key_routes_to_unknown() {
        let p = part(r#"{"videoMetadata":{"fps":24}}"#);
        match p.payload {
            PartPayload::Unknown { tag, .. } => assert_eq!(tag, "videoMetadata"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    // ── FinishReason ──────────────────────────────────────────────────

    #[test]
    fn finish_reason_known_variants_parse() {
        let cases = [
            ("STOP", FinishReason::Stop),
            ("MAX_TOKENS", FinishReason::MaxTokens),
            ("SAFETY", FinishReason::Safety),
            ("RECITATION", FinishReason::Recitation),
            ("LANGUAGE", FinishReason::Language),
            ("OTHER", FinishReason::Other),
            ("BLOCKLIST", FinishReason::Blocklist),
            ("PROHIBITED_CONTENT", FinishReason::ProhibitedContent),
            ("SPII", FinishReason::Spii),
            (
                "MALFORMED_FUNCTION_CALL",
                FinishReason::MalformedFunctionCall,
            ),
            ("IMAGE_SAFETY", FinishReason::ImageSafety),
        ];
        for (wire, expected) in cases {
            let parsed: FinishReason =
                serde_json::from_value(serde_json::Value::String(wire.to_owned())).unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(parsed.as_wire_str(), wire);
        }
    }

    #[test]
    fn finish_reason_unknown_round_trips() {
        let parsed = FinishReason::from_wire("FUTURE_REASON");
        assert_eq!(parsed, FinishReason::Unknown("FUTURE_REASON".to_owned()));
        assert_eq!(parsed.as_wire_str(), "FUTURE_REASON");
    }
}

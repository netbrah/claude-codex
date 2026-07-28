//! SSE parser for the Google Gemini `streamGenerateContent` wire protocol.
//!
//! Maps Gemini SSE data chunks into [`ResponseEvent`] so the rest of codex-rs
//! is wire-protocol agnostic. Unlike Anthropic's typed SSE events, Gemini
//! streams raw `data:` lines each containing a complete
//! `GenerateContentResponse` JSON object.

use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::error::ApiError;
use crate::sse::generate_content_wire_types::FinishReason;
use crate::sse::generate_content_wire_types::Part;
use crate::sse::generate_content_wire_types::PartPayload;
use codex_client::ByteStream;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;

use crate::sse::usage::RawUsage;
use crate::sse::usage::normalize_token_usage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::debug;
use tracing::trace;
use tracing::warn;

/// Curated catalogue of Gemini `streamGenerateContent` wire-vocabulary
/// strings XLI knows about. **This is rung-2 of the harness-invariant
/// ladder (see `cli-ops/sortie-board/xli-v3/`)** — a stringly-typed
/// table with a parser-fixture test, NOT the typed-enum refactor
/// (rung 3, tracked as `S-WIRE-VOCAB-MAX-TEETH`).
///
/// Source-of-truth for the wire vocabulary:
///   https://ai.google.dev/api/rest/v1beta/Content
///   https://ai.google.dev/api/rest/v1beta/GenerateContentResponse
///
/// **Superseded at runtime by rung 3** (`generate_content_wire_types`): the
/// typed `PartPayload` / `FinishReason` enums now own the parse-time
/// enforcement. This table is retained as a documentation catalogue and the
/// backing data for the drift-guard tests below, hence `allow(dead_code)`.
#[allow(dead_code)]
pub(crate) mod wire_vocab {
    /// `Part` discriminator. A Part is the payload union inside
    /// `candidates[].content.parts[]`.
    pub(crate) const PARTS: &[(&str, WirePolicy)] = &[
        ("text", WirePolicy::Handled),
        ("functionCall", WirePolicy::Handled),
        ("functionResponse", WirePolicy::Handled),
        ("thought", WirePolicy::Handled), // boolean flag on text parts; thoughtSignature handled alongside
        (
            "inlineData",
            WirePolicy::DropExplicit("base64-embedded media; not surfaced in XLI yet"),
        ),
        (
            "fileData",
            WirePolicy::DropExplicit("file-uri media; not surfaced in XLI yet"),
        ),
        (
            "executableCode",
            WirePolicy::DropExplicit(
                "server-side code-execution tool result; opt-in feature, not exposed by XLI",
            ),
        ),
        (
            "codeExecutionResult",
            WirePolicy::DropExplicit(
                "server-side code-execution tool result; opt-in feature, not exposed by XLI",
            ),
        ),
    ];

    /// `finishReason` on a candidate. We map known values to recoverable
    /// vs non-recoverable errors elsewhere; this table documents the
    /// known set.
    pub(crate) const FINISH_REASONS: &[(&str, WirePolicy)] = &[
        ("STOP", WirePolicy::Handled),
        ("MAX_TOKENS", WirePolicy::Handled),
        ("SAFETY", WirePolicy::Handled),
        ("RECITATION", WirePolicy::Handled),
        ("LANGUAGE", WirePolicy::Handled),
        ("OTHER", WirePolicy::Handled),
        ("BLOCKLIST", WirePolicy::Handled),
        ("PROHIBITED_CONTENT", WirePolicy::Handled),
        ("SPII", WirePolicy::Handled),
        ("MALFORMED_FUNCTION_CALL", WirePolicy::Handled),
        ("IMAGE_SAFETY", WirePolicy::Handled),
    ];

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum WirePolicy {
        Handled,
        DropExplicit(&'static str),
    }

    pub(crate) fn is_known_part(tag: &str) -> bool {
        PARTS.iter().any(|(k, _)| *k == tag)
    }

    pub(crate) fn is_known_finish_reason(tag: &str) -> bool {
        FINISH_REASONS.iter().any(|(k, _)| *k == tag)
    }
}

/// Monotonic counter for synthesizing unique call IDs.
///
/// Gemini's `functionCall` does not include an opaque call identifier
/// (unlike Anthropic's `tool_use.id`). We synthesize deterministic IDs
/// so `ResponseItem::FunctionCall.call_id` is always populated.
///
/// **Note:** This counter is process-global, not per-stream. Concurrent
/// Gemini streams in the same process will interleave IDs. This is fine
/// for correctness (call_id is only correlated within a single
/// conversation's `call_id_to_name` mapping) but IDs are not stable
/// across process restarts. If `call_id` is ever persisted or
/// cross-referenced post-restart, this should be scoped per-stream or
/// replaced with UUIDs.
static CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn generate_call_id() -> String {
    let n = CALL_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("gemini_call_{n}")
}

/// Top-level response from the Gemini streaming endpoint.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateContentResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default)]
    usage_metadata: Option<UsageMetadata>,
    #[serde(default)]
    model_version: Option<String>,
    #[serde(default)]
    response_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    #[serde(default)]
    content: Option<GeminiContent>,
    #[serde(default)]
    finish_reason: Option<FinishReason>,
    #[serde(default)]
    safety_ratings: Option<Vec<SafetyRating>>,
    /// Google Search grounding metadata. Present when `googleSearch` tool
    /// is active and the model used web grounding for the response.
    #[serde(default)]
    grounding_metadata: Option<GroundingMetadata>,
}

// ---------------------------------------------------------------------------
// groundingMetadata wire types (GS-3)
// ---------------------------------------------------------------------------

/// Top-level grounding metadata attached to a Gemini candidate.
///
/// Wire shape (camelCase from Gemini API):
/// ```json
/// {
///   "groundingChunks": [{"web": {"uri": "...", "title": "..."}}],
///   "groundingSupports": [{
///     "segment": {"startIndex": 0, "endIndex": 10, "text": "..."},
///     "groundingChunkIndices": [0],
///     "confidenceScores": [0.9]
///   }],
///   "webSearchQueries": ["query string"]
/// }
/// ```
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroundingMetadata {
    #[serde(default)]
    pub(crate) grounding_chunks: Vec<GroundingChunk>,
    #[serde(default)]
    pub(crate) grounding_supports: Vec<GroundingSupport>,
    #[serde(default)]
    pub(crate) web_search_queries: Vec<String>,
}

/// A single grounding source chunk (web page, document, etc.).
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroundingChunk {
    #[serde(default)]
    pub(crate) web: Option<GroundingChunkWeb>,
}

/// The web-specific payload inside a `GroundingChunk`.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroundingChunkWeb {
    pub(crate) uri: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
}

/// One segment of text supported by a set of grounding chunks.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroundingSupport {
    #[serde(default)]
    pub(crate) segment: Option<GroundingSegment>,
    #[serde(default)]
    pub(crate) grounding_chunk_indices: Vec<usize>,
}

/// Byte-offset span within the accumulated text that is supported by grounding.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroundingSegment {
    /// Byte offset of the start of the segment within the full text.
    /// Absent when the segment starts at 0.
    #[serde(default)]
    pub(crate) start_index: Option<u32>,
    /// Byte offset of the exclusive end of the segment.
    #[serde(default)]
    pub(crate) end_index: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct GeminiContent {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct UsageMetadata {
    #[serde(default)]
    prompt_token_count: Option<u32>,
    #[serde(default)]
    candidates_token_count: Option<u32>,
    #[serde(default)]
    total_token_count: Option<u32>,
    #[serde(default)]
    thoughts_token_count: Option<u32>,
    #[serde(default)]
    cached_content_token_count: Option<u32>,
    #[serde(default)]
    tool_use_prompt_token_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct SafetyRating {
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    probability: Option<String>,
    #[serde(default)]
    blocked: Option<bool>,
}

// ---------------------------------------------------------------------------
// Citation annotation (GS-3)
// ---------------------------------------------------------------------------

/// Post-processes the accumulated assistant text to insert inline citation
/// markers (`[N]`) at segment boundaries defined by `groundingSupports`, then
/// appends a `References:` block listing the cited sources.
///
/// ## Algorithm
///
/// 1. Walk `grounding_supports` sorted by segment `end_index` (ascending).
/// 2. For each support, collect the unique chunk indices it references.
/// 3. Build a global chunk → citation number map (first-seen wins,
///    so the same chunk always gets the same citation number).
/// 4. Insert `[N][M]…` markers immediately after each segment's end offset.
/// 5. Append a `\n\nReferences:\n[1] Title — URL\n…` block.
///
/// Returns the raw `text` unchanged when:
/// - `grounding_chunks` is empty (no sources to cite).
/// - `grounding_supports` is empty (no segment spans).
///
/// # Byte-offset semantics
///
/// Gemini's `segment.startIndex` / `endIndex` are **byte offsets** into the
/// UTF-8 text, not character offsets. We insert markers by slicing `&str`
/// at those offsets (safe because valid UTF-8 character boundaries produced
/// by the model), then concatenating the pieces.
pub(crate) fn annotate_with_citations(text: &str, meta: &GroundingMetadata) -> String {
    if meta.grounding_chunks.is_empty() || meta.grounding_supports.is_empty() {
        return text.to_string();
    }

    // Build chunk_index → citation_number map (1-based, first-seen order).
    // We derive the ordered list of chunks from groundingSupports so we only
    // number chunks that are actually cited.
    let mut chunk_to_num: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    let mut next_num: usize = 1;

    // Sort supports by end_index so we process text left-to-right.
    let mut supports = meta.grounding_supports.clone();
    supports.sort_by_key(|s| {
        s.segment
            .as_ref()
            .and_then(|seg| seg.end_index)
            .unwrap_or(0)
    });

    // First pass: assign citation numbers in the order segments appear.
    for support in &supports {
        for &idx in &support.grounding_chunk_indices {
            if idx < meta.grounding_chunks.len() {
                chunk_to_num.entry(idx).or_insert_with(|| {
                    let n = next_num;
                    next_num += 1;
                    n
                });
            }
        }
    }

    // Second pass: build the annotated text by inserting markers at segment
    // end offsets. We process right-to-left on a byte-level view so that
    // earlier insertions don't shift later offsets.
    //
    // Strategy: collect (end_byte_offset, marker_string) pairs, then
    // rebuild the string by walking the offset list left-to-right.
    let text_len = text.len();

    // Collect (end_offset, marker) — may have duplicates at the same offset.
    let mut insertions: Vec<(usize, String)> = Vec::new();
    for support in &supports {
        if support.grounding_chunk_indices.is_empty() {
            continue;
        }
        let end_offset = support
            .segment
            .as_ref()
            .and_then(|seg| seg.end_index)
            .map(|e| (e as usize).min(text_len))
            .unwrap_or(text_len);

        // Build marker like "[1][3]" for all chunk indices in this support.
        let mut marker = String::new();
        let mut cited: Vec<usize> = support
            .grounding_chunk_indices
            .iter()
            .filter(|&&idx| idx < meta.grounding_chunks.len())
            .filter_map(|&idx| chunk_to_num.get(&idx).copied())
            .collect();
        cited.sort_unstable();
        cited.dedup();
        for n in cited {
            marker.push_str(&format!("[{n}]"));
        }
        if !marker.is_empty() {
            insertions.push((end_offset, marker));
        }
    }

    // Sort insertions by offset ascending; within the same offset, stable-sort
    // preserves the order supports were processed (already sorted by end_index).
    insertions.sort_by_key(|(off, _)| *off);

    // Merge multiple markers at the same offset.
    let mut merged: Vec<(usize, String)> = Vec::new();
    for (off, marker) in insertions {
        if let Some(last) = merged.last_mut()
            && last.0 == off
        {
            last.1.push_str(&marker);
            continue;
        }
        merged.push((off, marker));
    }

    // Build the annotated text.
    let mut result = String::with_capacity(text.len() + merged.len() * 8);
    let mut cursor = 0usize;
    for (off, marker) in &merged {
        let end = (*off).min(text_len);
        // Clamp to a valid UTF-8 char boundary.
        let safe_end = if text.is_char_boundary(end) {
            end
        } else {
            // Walk back to the nearest valid boundary.
            (0..=end)
                .rev()
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(end)
        };
        if safe_end > cursor {
            result.push_str(&text[cursor..safe_end]);
        }
        result.push_str(marker);
        cursor = safe_end;
    }
    // Append any trailing text after the last insertion.
    if cursor < text_len {
        result.push_str(&text[cursor..]);
    }

    // Append reference list (sorted by citation number).
    let mut ref_entries: Vec<(usize, &GroundingChunk)> = chunk_to_num
        .iter()
        .map(|(&idx, &num)| (num, &meta.grounding_chunks[idx]))
        .collect();
    ref_entries.sort_by_key(|(num, _)| *num);

    result.push_str("\n\nReferences:");
    for (num, chunk) in ref_entries {
        if let Some(web) = &chunk.web {
            let title = web.title.as_deref().unwrap_or("Source");
            result.push_str(&format!("\n[{num}] {title} — {}", web.uri));
        }
    }

    result
}

/// Spawns a task that reads SSE events from a Gemini `streamGenerateContent`
/// byte stream and maps them into `ResponseEvent`s on the returned channel.
pub fn spawn_generate_content_stream(stream: ByteStream, idle_timeout: Duration) -> ResponseStream {
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(process_generate_content_sse(stream, tx_event, idle_timeout));
    ResponseStream {
        rx_event,
        upstream_request_id: None,
    }
}

/// Production entry point for the Gemini SSE parser when the full HTTP
/// response (including headers) is available.
///
/// Extracts the upstream request ID from the `x-request-id` response header
/// (set by a LiteLLM-compatible proxy) or the `x-goog-request-id` header (set by
/// the native Gemini API). Falls back to `None` gracefully when neither
/// header is present.
///
/// Delegates body parsing to [`spawn_generate_content_stream`].
pub fn spawn_generate_content_stream_from_response(
    stream_response: codex_client::StreamResponse,
    idle_timeout: Duration,
) -> ResponseStream {
    const REQUEST_ID_HEADER: &str = "x-request-id";
    const GOOG_REQUEST_ID_HEADER: &str = "x-goog-request-id";

    let upstream_request_id = stream_response
        .headers
        .get(REQUEST_ID_HEADER)
        .or_else(|| stream_response.headers.get(GOOG_REQUEST_ID_HEADER))
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let mut rs = spawn_generate_content_stream(stream_response.bytes, idle_timeout);
    rs.upstream_request_id = upstream_request_id;
    rs
}

async fn process_generate_content_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
) {
    let mut sse_stream = stream.eventsource();
    let mut response_id = String::new();
    let mut final_usage: Option<TokenUsage> = None;
    let mut final_stop_reason: Option<String> = None;
    let mut emitted_created = false;
    let mut emitted_item_added = false;
    let mut accumulated_text = String::new();
    let mut emitted_reasoning_item = false;
    let mut truncation_warned = false;
    // GS-3: accumulate grounding metadata across all candidate chunks.
    // Gemini typically delivers the full groundingMetadata on the last chunk,
    // but we merge across all chunks for robustness.
    let mut accumulated_grounding: GroundingMetadata = GroundingMetadata::default();

    loop {
        let next = timeout(idle_timeout, sse_stream.next()).await;
        match next {
            Err(_elapsed) => {
                // If we already received a finishReason, the stream ended
                // naturally and the timeout fired on the trailing EOF poll.
                // Only report an error if the stream stalled mid-response.
                if final_stop_reason.is_none() {
                    let _ = tx_event
                        .send(Err(ApiError::Stream(
                            "Gemini stream idle timeout".to_string(),
                        )))
                        .await;
                }
                break;
            }
            Ok(None) => break,
            Ok(Some(Err(e))) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(format!("Gemini SSE error: {e}"))))
                    .await;
                return;
            }
            Ok(Some(Ok(event))) => {
                let data = event.data;
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                trace!(data = %data, "gemini SSE chunk");

                let chunk: GenerateContentResponse = match serde_json::from_str(&data) {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(error = %e, data = %data, "failed to parse Gemini SSE chunk");
                        continue;
                    }
                };

                if !emitted_created {
                    emitted_created = true;
                    let _ = tx_event.send(Ok(ResponseEvent::Created)).await;
                }

                // Use Gemini's responseId as the canonical response identifier.
                // Falls back to a synthetic ID if not present.
                if let Some(rid) = &chunk.response_id
                    && response_id.is_empty()
                {
                    response_id = rid.clone();
                }

                // Process candidates (we only use candidates[0]).
                if let Some(candidate) = chunk.candidates.first() {
                    // Check for safety block.
                    if let Some(ratings) = &candidate.safety_ratings {
                        for rating in ratings {
                            if rating.blocked == Some(true) {
                                let category = rating.category.as_deref().unwrap_or("UNKNOWN");
                                let _ = tx_event
                                    .send(Err(ApiError::Stream(format!(
                                        "Gemini safety filter blocked response: {category}"
                                    ))))
                                    .await;
                                return;
                            }
                        }
                    }

                    // Process content parts.
                    if let Some(content) = &candidate.content {
                        for part in &content.parts {
                            // Thinking parts carry `thought: true`. Their
                            // payload (when present) is a text string; emit it
                            // as reasoning rather than assistant output.
                            if part.thought {
                                let text = if let PartPayload::Text(t) = &part.payload {
                                    t.as_str()
                                } else {
                                    ""
                                };
                                if !text.is_empty() {
                                    if !emitted_reasoning_item {
                                        emitted_reasoning_item = true;
                                        let item = ResponseItem::Reasoning {
                                            id: Some("gemini_reasoning_0".to_string()),
                                            summary: vec![],
                                            content: None,
                                            encrypted_content: None,
                                            internal_chat_message_metadata_passthrough: None,
                                            raw_wire_block: None,
                                        };
                                        let _ = tx_event
                                            .send(Ok(ResponseEvent::OutputItemAdded(item)))
                                            .await;
                                    }
                                    let _ = tx_event
                                        .send(Ok(ResponseEvent::ReasoningSummaryDelta {
                                            delta: text.to_string(),
                                            summary_index: 0,
                                        }))
                                        .await;
                                }
                                // Stash thoughtSignature on thinking text parts
                                // for round-trip via Reasoning raw_wire_block.
                                if part.thought_signature.is_some() {
                                    let raw = serde_json::json!({
                                        "text": text,
                                        "thoughtSignature": part.thought_signature,
                                    });
                                    let item = ResponseItem::Reasoning {
                                        id: Some("gemini_reasoning_0".to_string()),
                                        summary: vec![],
                                        content: None,
                                        encrypted_content: None,
                                        internal_chat_message_metadata_passthrough: None,
                                        raw_wire_block: Some(raw),
                                    };
                                    let _ = tx_event
                                        .send(Ok(ResponseEvent::OutputItemDone(item)))
                                        .await;
                                }
                                continue;
                            }

                            // Exhaustive match over the typed part payload. A
                            // never-before-seen Gemini part key lands in
                            // `PartPayload::Unknown` (already warned at
                            // deserialize time) instead of a silent drop.
                            match &part.payload {
                                PartPayload::Text(text) => {
                                    if text.is_empty() {
                                        continue;
                                    }
                                    if !emitted_item_added {
                                        emitted_item_added = true;
                                        let item = ResponseItem::Message {
                                            id: None,
                                            role: "assistant".to_string(),
                                            content: vec![],
                                            phase: None,
                                        };
                                        let _ = tx_event
                                            .send(Ok(ResponseEvent::OutputItemAdded(item)))
                                            .await;
                                    }
                                    const MAX_ACCUMULATED_BYTES: usize = 4 * 1024 * 1024;
                                    if accumulated_text.len() + text.len() <= MAX_ACCUMULATED_BYTES
                                    {
                                        accumulated_text.push_str(text);
                                    } else if !truncation_warned {
                                        truncation_warned = true;
                                        warn!(
                                            cap_bytes = MAX_ACCUMULATED_BYTES,
                                            "gemini accumulated text exceeded 4MB cap; \
                                                 OutputItemDone will be truncated"
                                        );
                                    }
                                    let _ = tx_event
                                        .send(Ok(ResponseEvent::OutputTextDelta(text.clone())))
                                        .await;
                                }
                                PartPayload::FunctionCall(fc) => {
                                    // When thoughtSignature is present, emit a
                                    // Reasoning item with raw_wire_block carrying
                                    // the entire part JSON. This is echoed back
                                    // verbatim on egress so the Gemini API can
                                    // verify the model's reasoning chain.
                                    if let Some(sig) = &part.thought_signature {
                                        let raw = serde_json::json!({
                                            "functionCall": {
                                                "name": fc.name,
                                                "args": fc.args,
                                            },
                                            "thoughtSignature": sig,
                                        });
                                        let item = ResponseItem::Reasoning {
                                            id: Some(format!(
                                                "gemini_sig_{}",
                                                CALL_COUNTER.load(Ordering::Relaxed)
                                            )),
                                            summary: vec![],
                                            content: None,
                                            encrypted_content: None,
                                            internal_chat_message_metadata_passthrough: None,
                                            raw_wire_block: Some(raw),
                                        };
                                        let _ = tx_event
                                            .send(Ok(ResponseEvent::OutputItemAdded(item.clone())))
                                            .await;
                                        let _ = tx_event
                                            .send(Ok(ResponseEvent::OutputItemDone(item)))
                                            .await;
                                    }

                                    let call_id = generate_call_id();
                                    let arguments = fc
                                        .args
                                        .as_ref()
                                        .map(|a| serde_json::to_string(a).unwrap_or_default())
                                        .unwrap_or_else(|| "{}".to_string());
                                    let (namespace, bare_name) =
                                        crate::sse::messages::parse_flat_mcp_tool_name_pub(
                                            &fc.name,
                                        );
                                    let item = ResponseItem::FunctionCall {
                                        id: None,
                                        call_id,
                                        name: bare_name,
                                        arguments,
                                        namespace,
                                    };
                                    let _ = tx_event
                                        .send(Ok(ResponseEvent::OutputItemAdded(item.clone())))
                                        .await;
                                    let _ = tx_event
                                        .send(Ok(ResponseEvent::OutputItemDone(item)))
                                        .await;
                                }
                                // Media / server-side-tool payloads XLI does not
                                // surface. Named and dropped per wire_vocab::PARTS
                                // policy — NOT a silent serde drop.
                                PartPayload::CodeExecutionResult(_) => {
                                    trace!(
                                        "gemini codeExecutionResult dropped per wire_vocab::PARTS policy"
                                    );
                                }
                                PartPayload::ExecutableCode(_) => {
                                    trace!(
                                        "gemini executableCode dropped per wire_vocab::PARTS policy"
                                    );
                                }
                                PartPayload::FunctionResponse(_) => {
                                    trace!(
                                        "gemini functionResponse dropped per wire_vocab::PARTS policy"
                                    );
                                }
                                PartPayload::InlineData(_) => {
                                    trace!(
                                        "gemini inlineData dropped per wire_vocab::PARTS policy"
                                    );
                                }
                                PartPayload::FileData(_) => {
                                    trace!("gemini fileData dropped per wire_vocab::PARTS policy");
                                }
                                // Modifier-only / empty part (e.g. a bare
                                // thoughtSignature with no thought:true) — nothing
                                // to surface here.
                                PartPayload::Empty => {}
                                PartPayload::Unknown { tag, .. } => {
                                    warn!(
                                        part_key = %tag,
                                        "gemini Part key NOT in wire_vocab::PARTS — routed to PartPayload::Unknown"
                                    );
                                }
                            }
                        }
                    }

                    // GS-3: Capture grounding metadata from this chunk.
                    // Gemini delivers the full groundingMetadata in the final
                    // candidate chunk. We overwrite rather than merge because
                    // the last chunk's metadata supersedes earlier partials.
                    if let Some(gm) = &candidate.grounding_metadata {
                        if !gm.grounding_chunks.is_empty() {
                            accumulated_grounding.grounding_chunks = gm.grounding_chunks.clone();
                        }
                        if !gm.grounding_supports.is_empty() {
                            accumulated_grounding.grounding_supports =
                                gm.grounding_supports.clone();
                        }
                        if !gm.web_search_queries.is_empty() {
                            accumulated_grounding.web_search_queries =
                                gm.web_search_queries.clone();
                        }
                    }

                    // Capture finish reason. SAFETY/RECITATION are non-recoverable
                    // errors — emit as stream errors rather than Completed events
                    // to match the safety_ratings.blocked handling above.
                    if let Some(reason) = &candidate.finish_reason {
                        // Exhaustive match over the typed FinishReason. New
                        // upstream terminal states arrive as
                        // FinishReason::Unknown (warned at deserialize time)
                        // and fall through to the graceful stop-reason path
                        // rather than a silent `_ =>` arm.
                        match reason {
                            FinishReason::Safety => {
                                // Include specific blocked categories when available
                                // so operators can see which safety filter fired.
                                let blocked_categories: Vec<&str> = candidate
                                    .safety_ratings
                                    .as_ref()
                                    .map(|ratings| {
                                        ratings
                                            .iter()
                                            .filter(|r| r.blocked == Some(true))
                                            .filter_map(|r| r.category.as_deref())
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                let msg = if blocked_categories.is_empty() {
                                    "Gemini response blocked: SAFETY".to_string()
                                } else {
                                    format!(
                                        "Gemini safety filter blocked response ({})",
                                        blocked_categories.join(", ")
                                    )
                                };
                                let _ = tx_event.send(Err(ApiError::Stream(msg))).await;
                                return;
                            }
                            FinishReason::Recitation
                            | FinishReason::Blocklist
                            | FinishReason::ProhibitedContent
                            | FinishReason::Spii
                            | FinishReason::ImageSafety => {
                                let _ = tx_event
                                    .send(Err(ApiError::Stream(format!(
                                        "Gemini response blocked: {}",
                                        reason.as_wire_str()
                                    ))))
                                    .await;
                                return;
                            }
                            FinishReason::MalformedFunctionCall => {
                                let _ = tx_event
                                    .send(Err(ApiError::Stream(
                                        "Gemini emitted malformed function call \
                                         (likely schema mismatch)"
                                            .to_string(),
                                    )))
                                    .await;
                                return;
                            }
                            FinishReason::Language => {
                                let _ = tx_event
                                    .send(Err(ApiError::Stream(
                                        "Gemini refused: unsupported language".to_string(),
                                    )))
                                    .await;
                                return;
                            }
                            FinishReason::Stop
                            | FinishReason::MaxTokens
                            | FinishReason::Other
                            | FinishReason::Unknown(_) => {
                                final_stop_reason = Some(gemini_finish_to_stop_reason(reason));
                            }
                        }
                    }
                }

                // Capture usage metadata.
                if let Some(usage) = &chunk.usage_metadata {
                    final_usage = Some(normalize_token_usage(RawUsage::cache_inclusive_prompt(
                        i64::from(usage.prompt_token_count.unwrap_or(0)),
                        i64::from(usage.cached_content_token_count.unwrap_or(0)),
                        0,
                        i64::from(usage.candidates_token_count.unwrap_or(0)),
                        i64::from(usage.thoughts_token_count.unwrap_or(0)),
                        i64::from(usage.total_token_count.unwrap_or(0)),
                    )));
                }
            }
        }
    }

    // Generate a synthetic response_id if Gemini did not provide one.
    // In practice, Gemini always returns `responseId` on every chunk,
    // so this is a defensive fallback. The wall-clock nanos value is
    // unique per stream, which is sufficient for telemetry correlation.
    // TODO(Phase 3): replace with a deterministic per-request hash if
    // replay-stable IDs become a requirement.
    if response_id.is_empty() {
        response_id = format!(
            "gemini-{:016x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
        );
    }

    // Emit OutputItemDone with the (optionally citation-annotated) accumulated
    // text so downstream consumers (TUI, session state) can finalize the
    // message item — mirrors the Anthropic SSE parser's content_block_stop.
    //
    // GS-3: if the response carried groundingMetadata, apply
    // `annotate_with_citations` to insert [N] markers and a References block
    // before handing the text off to the protocol layer.
    if !accumulated_text.is_empty() {
        let final_text = if !accumulated_grounding.grounding_chunks.is_empty() {
            annotate_with_citations(&accumulated_text, &accumulated_grounding)
        } else {
            accumulated_text
        };
        let message_item = codex_protocol::models::ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![codex_protocol::models::ContentItem::OutputText { text: final_text }],
            phase: None,
        };
        let _ = tx_event
            .send(Ok(ResponseEvent::OutputItemDone(message_item)))
            .await;
    }

    debug!(
        stop_reason = ?final_stop_reason,
        usage = ?final_usage,
        "gemini stream completed"
    );

    let _ = tx_event
        .send(Ok({
            let end_turn = final_stop_reason.as_deref().map(|r| r == "end_turn");
            ResponseEvent::Completed {
                stop_reason: final_stop_reason,
                response_id,
                token_usage: final_usage,
                end_turn,
            }
        }))
        .await;
}

/// Maps a typed Gemini [`FinishReason`] to the stop_reason string used by
/// codex-rs internally (matching Anthropic conventions). Unknown reasons are
/// lower-cased verbatim.
fn gemini_finish_to_stop_reason(reason: &FinishReason) -> String {
    match reason {
        FinishReason::Stop => "end_turn".to_string(),
        FinishReason::MaxTokens => "max_tokens".to_string(),
        FinishReason::Safety => "safety".to_string(),
        FinishReason::Recitation => "recitation".to_string(),
        FinishReason::MalformedFunctionCall => "malformed_function_call".to_string(),
        FinishReason::Language => "language".to_string(),
        FinishReason::Other => "other".to_string(),
        FinishReason::Blocklist => "blocklist".to_string(),
        FinishReason::ProhibitedContent => "prohibited_content".to_string(),
        FinishReason::Spii => "spii".to_string(),
        FinishReason::ImageSafety => "image_safety".to_string(),
        FinishReason::Unknown(other) => other.to_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ContentItem;

    fn make_sse_bytes(events: &[&str]) -> ByteStream {
        let mut data = String::new();
        for event in events {
            data.push_str(&format!("data: {event}\n\n"));
        }
        let bytes = bytes::Bytes::from(data);
        let stream = futures::stream::once(async move { Ok(bytes) });
        Box::pin(stream)
    }

    /// Rung-2 invariant: the curated `wire_vocab` tables must stay
    /// consistent with the parser. See the analogous test in
    /// `messages.rs` for the full rationale.
    /// Replaced by typed-enum exhaustiveness in rung 3
    /// (`S-WIRE-VOCAB-MAX-TEETH`).
    #[test]
    fn wire_vocab_drop_entries_have_rationale() {
        let all = [wire_vocab::PARTS, wire_vocab::FINISH_REASONS];
        for table in all.iter() {
            for (tag, policy) in *table {
                if let wire_vocab::WirePolicy::DropExplicit(reason) = policy {
                    assert!(
                        !reason.trim().is_empty(),
                        "wire_vocab `{tag}` is DropExplicit but rationale is empty"
                    );
                }
            }
        }
    }

    /// Every FINISH_REASONS entry must appear at least once as a string
    /// literal in the parser file, otherwise the table has drifted from
    /// reality.
    #[test]
    fn wire_vocab_finish_reasons_referenced() {
        let source = include_str!("generate_content.rs");
        for (tag, _) in wire_vocab::FINISH_REASONS {
            assert!(
                source.contains(&format!(r#""{tag}""#)),
                "wire_vocab::FINISH_REASONS entry `{tag}` is not referenced \
                 anywhere in generate_content.rs."
            );
        }
    }

    #[tokio::test]
    async fn happy_path_text_streaming() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":"Hello"}],"role":"model"}}]}"#,
            r#"{"candidates":[{"content":{"parts":[{"text":" world!"}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":3,"totalTokenCount":8}}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut collected = Vec::new();
        while let Some(event) = response.rx_event.recv().await {
            collected.push(event.unwrap());
        }

        assert!(matches!(collected[0], ResponseEvent::Created));
        // OutputItemAdded(Message) before first text delta.
        assert!(matches!(
            collected[1],
            ResponseEvent::OutputItemAdded(ResponseItem::Message { .. })
        ));
        assert!(matches!(collected[2], ResponseEvent::OutputTextDelta(ref t) if t == "Hello"));
        assert!(matches!(collected[3], ResponseEvent::OutputTextDelta(ref t) if t == " world!"));
        // OutputItemDone with accumulated text precedes Completed.
        match &collected[4] {
            ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) => {
                match &content[0] {
                    ContentItem::OutputText { text } => assert_eq!(text, "Hello world!"),
                    other => panic!("expected OutputText, got {other:?}"),
                }
            }
            other => panic!("expected OutputItemDone, got {other:?}"),
        }
        match &collected[5] {
            ResponseEvent::Completed {
                stop_reason,
                token_usage,
                ..
            } => {
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
                let usage = token_usage.as_ref().unwrap();
                assert_eq!(usage.input_tokens, 5);
                assert_eq!(usage.output_tokens, 3);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_stream_produces_completed() {
        let stream = make_sse_bytes(&[]);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut collected = Vec::new();
        while let Some(event) = response.rx_event.recv().await {
            collected.push(event.unwrap());
        }

        // No events sent, but Completed should still be emitted.
        assert!(matches!(
            collected.last().unwrap(),
            ResponseEvent::Completed { .. }
        ));
    }

    #[tokio::test]
    async fn safety_block_produces_error() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":""}],"role":"model"},"safetyRatings":[{"category":"HARM_CATEGORY_HARASSMENT","probability":"HIGH","blocked":true}]}]}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut found_error = false;
        while let Some(event) = response.rx_event.recv().await {
            if let Err(ApiError::Stream(msg)) = event {
                assert!(msg.contains("safety filter"));
                found_error = true;
                break;
            }
        }
        assert!(found_error, "expected safety block error");
    }

    #[tokio::test]
    async fn finish_reason_mapping() {
        assert_eq!(
            gemini_finish_to_stop_reason(&FinishReason::Stop),
            "end_turn"
        );
        assert_eq!(
            gemini_finish_to_stop_reason(&FinishReason::MaxTokens),
            "max_tokens"
        );
        assert_eq!(
            gemini_finish_to_stop_reason(&FinishReason::Safety),
            "safety"
        );
        assert_eq!(
            gemini_finish_to_stop_reason(&FinishReason::Recitation),
            "recitation"
        );
        assert_eq!(
            gemini_finish_to_stop_reason(&FinishReason::MalformedFunctionCall),
            "malformed_function_call"
        );
        assert_eq!(gemini_finish_to_stop_reason(&FinishReason::Other), "other");
    }

    #[tokio::test]
    async fn finish_reason_safety_produces_error() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":"partial"}],"role":"model"},"finishReason":"SAFETY"}]}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut found_error = false;
        while let Some(event) = response.rx_event.recv().await {
            if let Err(ApiError::Stream(msg)) = event {
                assert!(msg.contains("SAFETY"), "error should mention SAFETY: {msg}");
                found_error = true;
                break;
            }
        }
        assert!(
            found_error,
            "finishReason SAFETY should produce a stream error"
        );
    }

    #[tokio::test]
    async fn finish_reason_recitation_produces_error() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":"copied"}],"role":"model"},"finishReason":"RECITATION"}]}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut found_error = false;
        while let Some(event) = response.rx_event.recv().await {
            if let Err(ApiError::Stream(msg)) = event {
                assert!(msg.contains("RECITATION"));
                found_error = true;
                break;
            }
        }
        assert!(
            found_error,
            "finishReason RECITATION should produce a stream error"
        );
    }

    #[tokio::test]
    async fn max_tokens_finish_reason() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":"truncated"}],"role":"model"},"finishReason":"MAX_TOKENS"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":100,"totalTokenCount":110}}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut collected = Vec::new();
        while let Some(event) = response.rx_event.recv().await {
            collected.push(event.unwrap());
        }

        match &collected.last().unwrap() {
            ResponseEvent::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("max_tokens"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn single_function_call() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"shell","args":{"command":["echo","hello"]}}}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"totalTokenCount":15}}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut collected = Vec::new();
        while let Some(event) = response.rx_event.recv().await {
            collected.push(event.unwrap());
        }

        // Created, OutputItemAdded(FunctionCall), OutputItemDone(FunctionCall), Completed
        assert!(matches!(collected[0], ResponseEvent::Created));
        match &collected[1] {
            ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            }) => {
                assert_eq!(name, "shell");
                assert!(call_id.starts_with("gemini_call_"));
                let args: serde_json::Value = serde_json::from_str(arguments).unwrap();
                assert_eq!(args["command"], serde_json::json!(["echo", "hello"]));
            }
            other => panic!("expected OutputItemAdded(FunctionCall), got {other:?}"),
        }
        assert!(matches!(
            &collected[2],
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { .. })
        ));
        assert!(matches!(&collected[3], ResponseEvent::Completed { .. }));
    }

    #[tokio::test]
    async fn parallel_function_calls() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"shell","args":{"command":["ls"]}}},{"functionCall":{"name":"shell","args":{"command":["pwd"]}}}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":8,"totalTokenCount":18}}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut collected = Vec::new();
        while let Some(event) = response.rx_event.recv().await {
            collected.push(event.unwrap());
        }

        // Created, then for each call: OutputItemAdded + OutputItemDone, then Completed
        assert!(matches!(collected[0], ResponseEvent::Created));
        // First call
        match &collected[1] {
            ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall { name, .. }) => {
                assert_eq!(name, "shell");
            }
            other => panic!("expected first FunctionCall, got {other:?}"),
        }
        assert!(matches!(
            &collected[2],
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { .. })
        ));
        // Second call
        match &collected[3] {
            ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall { name, .. }) => {
                assert_eq!(name, "shell");
            }
            other => panic!("expected second FunctionCall, got {other:?}"),
        }
        assert!(matches!(
            &collected[4],
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { .. })
        ));
        // Different call_ids
        let id1 = match &collected[1] {
            ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall { call_id, .. }) => {
                call_id.clone()
            }
            _ => unreachable!(),
        };
        let id2 = match &collected[3] {
            ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall { call_id, .. }) => {
                call_id.clone()
            }
            _ => unreachable!(),
        };
        assert_ne!(id1, id2, "parallel calls must have distinct call_ids");
        assert!(matches!(&collected[5], ResponseEvent::Completed { .. }));
    }

    #[tokio::test]
    async fn text_then_function_call() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":"Let me run that for you."},{"functionCall":{"name":"shell","args":{"command":["echo","hi"]}}}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":12,"totalTokenCount":22}}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut collected = Vec::new();
        while let Some(event) = response.rx_event.recv().await {
            collected.push(event.unwrap());
        }

        // Created, OutputItemAdded(Message), OutputTextDelta, OutputItemAdded(FunctionCall),
        // OutputItemDone(FunctionCall), OutputItemDone(Message with text), Completed
        assert!(matches!(collected[0], ResponseEvent::Created));
        assert!(matches!(
            &collected[1],
            ResponseEvent::OutputItemAdded(ResponseItem::Message { .. })
        ));
        match &collected[2] {
            ResponseEvent::OutputTextDelta(t) => assert_eq!(t, "Let me run that for you."),
            other => panic!("expected OutputTextDelta, got {other:?}"),
        }
        assert!(matches!(
            &collected[3],
            ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall { .. })
        ));
        assert!(matches!(
            &collected[4],
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { .. })
        ));
    }

    #[tokio::test]
    async fn function_call_empty_args() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"get_time","args":{}}}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":3,"totalTokenCount":8}}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut collected = Vec::new();
        while let Some(event) = response.rx_event.recv().await {
            collected.push(event.unwrap());
        }

        match &collected[1] {
            ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall {
                name, arguments, ..
            }) => {
                assert_eq!(name, "get_time");
                assert_eq!(arguments, "{}");
            }
            other => panic!("expected FunctionCall with empty args, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_function_call_produces_error() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"shell","args":{}}}],"role":"model"},"finishReason":"MALFORMED_FUNCTION_CALL"}]}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut found_error = false;
        while let Some(event) = response.rx_event.recv().await {
            if let Err(ApiError::Stream(msg)) = event {
                assert!(
                    msg.contains("malformed function call"),
                    "error should mention malformed function call: {msg}"
                );
                found_error = true;
                break;
            }
        }
        assert!(
            found_error,
            "MALFORMED_FUNCTION_CALL should produce a stream error, not Completed"
        );
    }

    #[tokio::test]
    async fn no_tool_call_input_delta_emitted() {
        // Regression: Gemini sends complete args — ToolCallInputDelta must
        // never appear. Verify only OutputItemAdded + OutputItemDone.
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"shell","args":{"command":["ls"]}}}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":3,"totalTokenCount":8}}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        while let Some(event) = response.rx_event.recv().await {
            let event = event.unwrap();
            // ToolCallInputDelta doesn't exist as a variant, but verify
            // no OutputTextDelta is emitted for tool args.
            if let ResponseEvent::OutputTextDelta(ref _t) = event {
                panic!("OutputTextDelta should not be emitted for function call args");
            }
        }
    }

    #[tokio::test]
    async fn blocklist_finish_reason_produces_error() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":"x"}],"role":"model"},"finishReason":"BLOCKLIST"}]}"#,
        ];
        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));
        let mut found_error = false;
        while let Some(event) = response.rx_event.recv().await {
            if let Err(ApiError::Stream(msg)) = event {
                assert!(msg.contains("BLOCKLIST"));
                found_error = true;
                break;
            }
        }
        assert!(found_error, "BLOCKLIST should produce a stream error");
    }

    #[tokio::test]
    async fn prohibited_content_finish_reason_produces_error() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":"x"}],"role":"model"},"finishReason":"PROHIBITED_CONTENT"}]}"#,
        ];
        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));
        let mut found_error = false;
        while let Some(event) = response.rx_event.recv().await {
            if let Err(ApiError::Stream(msg)) = event {
                assert!(msg.contains("PROHIBITED_CONTENT"));
                found_error = true;
                break;
            }
        }
        assert!(
            found_error,
            "PROHIBITED_CONTENT should produce a stream error"
        );
    }

    #[tokio::test]
    async fn finish_reason_language_produces_error() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":"x"}],"role":"model"},"finishReason":"LANGUAGE"}]}"#,
        ];
        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));
        let mut found_error = false;
        while let Some(event) = response.rx_event.recv().await {
            if let Err(ApiError::Stream(msg)) = event {
                assert!(msg.contains("unsupported language"));
                found_error = true;
                break;
            }
        }
        assert!(found_error, "LANGUAGE should produce a stream error");
    }

    #[tokio::test]
    async fn safety_finish_reason_includes_blocked_category() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":"x"}],"role":"model"},"finishReason":"SAFETY","safetyRatings":[{"category":"HARM_CATEGORY_DANGEROUS_CONTENT","probability":"HIGH","blocked":true},{"category":"HARM_CATEGORY_HARASSMENT","probability":"LOW","blocked":false}]}]}"#,
        ];
        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));
        let mut found_error = false;
        while let Some(event) = response.rx_event.recv().await {
            if let Err(ApiError::Stream(msg)) = event {
                assert!(
                    msg.contains("HARM_CATEGORY_DANGEROUS_CONTENT"),
                    "error should include blocked category: {msg}"
                );
                assert!(
                    !msg.contains("HARM_CATEGORY_HARASSMENT"),
                    "error should not include non-blocked category: {msg}"
                );
                found_error = true;
                break;
            }
        }
        assert!(
            found_error,
            "SAFETY with ratings should produce a detailed error"
        );
    }

    #[tokio::test]
    async fn safety_finish_reason_without_ratings_falls_back() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":"x"}],"role":"model"},"finishReason":"SAFETY"}]}"#,
        ];
        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));
        let mut found_error = false;
        while let Some(event) = response.rx_event.recv().await {
            if let Err(ApiError::Stream(msg)) = event {
                assert_eq!(msg, "Gemini response blocked: SAFETY");
                found_error = true;
                break;
            }
        }
        assert!(
            found_error,
            "SAFETY without ratings should fall back to generic message"
        );
    }

    #[tokio::test]
    async fn thinking_parts_emit_reasoning_events() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":"Let me think...","thought":true}],"role":"model"}}]}"#,
            r#"{"candidates":[{"content":{"parts":[{"text":"Step 2...","thought":true}],"role":"model"}}]}"#,
            r#"{"candidates":[{"content":{"parts":[{"text":"The answer is 42."}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":20,"totalTokenCount":25}}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut collected = Vec::new();
        while let Some(event) = response.rx_event.recv().await {
            collected.push(event.unwrap());
        }

        // Created, OutputItemAdded(Reasoning), ReasoningSummaryDelta x2,
        // OutputItemAdded(Message), OutputTextDelta, OutputItemDone(Message), Completed
        assert!(matches!(collected[0], ResponseEvent::Created));
        assert!(matches!(
            &collected[1],
            ResponseEvent::OutputItemAdded(ResponseItem::Reasoning { .. })
        ));
        assert!(matches!(
            &collected[2],
            ResponseEvent::ReasoningSummaryDelta { .. }
        ));
        assert!(matches!(
            &collected[3],
            ResponseEvent::ReasoningSummaryDelta { .. }
        ));
        assert!(matches!(
            &collected[4],
            ResponseEvent::OutputItemAdded(ResponseItem::Message { .. })
        ));
        assert!(
            matches!(&collected[5], ResponseEvent::OutputTextDelta(t) if t == "The answer is 42.")
        );
    }

    #[tokio::test]
    async fn thought_signature_on_function_call_emits_reasoning_block() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"shell","args":{"command":["echo","hello"]}},"thoughtSignature":"SIG_ABC123"}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"totalTokenCount":15}}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut collected = Vec::new();
        while let Some(event) = response.rx_event.recv().await {
            collected.push(event.unwrap());
        }

        // Should emit: Created, OutputItemAdded(Reasoning with raw_wire_block),
        //              OutputItemDone(Reasoning), OutputItemAdded(FunctionCall),
        //              OutputItemDone(FunctionCall), Completed
        assert!(matches!(collected[0], ResponseEvent::Created));

        // Reasoning block carries the thoughtSignature + functionCall
        let reasoning_item = collected.iter().find(|e| {
            matches!(
                e,
                ResponseEvent::OutputItemAdded(ResponseItem::Reasoning {
                    internal_chat_message_metadata_passthrough: None,

                    raw_wire_block: Some(_),
                    ..
                })
            )
        });
        assert!(
            reasoning_item.is_some(),
            "should emit Reasoning with raw_wire_block"
        );

        if let ResponseEvent::OutputItemAdded(ResponseItem::Reasoning {
            internal_chat_message_metadata_passthrough: None,

            raw_wire_block: Some(block),
            ..
        }) = reasoning_item.unwrap()
        {
            assert!(
                block.get("thoughtSignature").is_some(),
                "raw_wire_block should contain thoughtSignature"
            );
            assert!(
                block.get("functionCall").is_some(),
                "raw_wire_block should contain functionCall"
            );
            assert_eq!(block["thoughtSignature"], "SIG_ABC123");
        }

        // FunctionCall should still be emitted for the agent loop
        let has_function_call = collected.iter().any(|e| {
            matches!(
                e,
                ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall { .. })
            )
        });
        assert!(
            has_function_call,
            "should still emit FunctionCall for agent loop"
        );
    }

    // -----------------------------------------------------------------------
    // GS-3: groundingMetadata deserialization + citation annotation tests
    // -----------------------------------------------------------------------

    #[test]
    fn grounding_metadata_deserializes_from_fixture() {
        let json = r#"{
            "groundingChunks": [
                {"web": {"uri": "https://example.com/page1", "title": "Example Page"}},
                {"web": {"uri": "https://other.org/doc", "title": "Other Doc"}}
            ],
            "groundingSupports": [
                {
                    "segment": {"startIndex": 0, "endIndex": 12},
                    "groundingChunkIndices": [0],
                    "confidenceScores": [0.95]
                },
                {
                    "segment": {"startIndex": 13, "endIndex": 25},
                    "groundingChunkIndices": [1],
                    "confidenceScores": [0.85]
                }
            ],
            "webSearchQueries": ["what is Rust 1.85"]
        }"#;
        let gm: GroundingMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(gm.grounding_chunks.len(), 2);
        assert_eq!(gm.grounding_supports.len(), 2);
        assert_eq!(gm.web_search_queries.len(), 1);
        assert_eq!(gm.web_search_queries[0], "what is Rust 1.85");
        let chunk0_web = gm.grounding_chunks[0].web.as_ref().unwrap();
        assert_eq!(chunk0_web.uri, "https://example.com/page1");
        assert_eq!(chunk0_web.title.as_deref(), Some("Example Page"));
    }

    #[test]
    fn grounding_metadata_deserializes_without_title() {
        let json = r#"{
            "groundingChunks": [{"web": {"uri": "https://example.com"}}],
            "groundingSupports": [],
            "webSearchQueries": []
        }"#;
        let gm: GroundingMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(gm.grounding_chunks.len(), 1);
        let web = gm.grounding_chunks[0].web.as_ref().unwrap();
        assert!(web.title.is_none());
    }

    #[test]
    fn annotate_with_citations_inserts_markers_and_references() {
        // text: "Hello world! Goodbye world." (28 bytes)
        //        0123456789012345678901234567
        // Support 1: [0,12) -> chunk 0 -> [1]
        // Support 2: [13,27) -> chunk 1 -> [2]
        let meta = GroundingMetadata {
            grounding_chunks: vec![
                GroundingChunk {
                    web: Some(GroundingChunkWeb {
                        uri: "https://example.com/page1".to_string(),
                        title: Some("Example Page".to_string()),
                    }),
                },
                GroundingChunk {
                    web: Some(GroundingChunkWeb {
                        uri: "https://other.org/doc".to_string(),
                        title: Some("Other Doc".to_string()),
                    }),
                },
            ],
            grounding_supports: vec![
                GroundingSupport {
                    segment: Some(GroundingSegment {
                        start_index: Some(0),
                        end_index: Some(12),
                    }),
                    grounding_chunk_indices: vec![0],
                },
                GroundingSupport {
                    segment: Some(GroundingSegment {
                        start_index: Some(13),
                        end_index: Some(27),
                    }),
                    grounding_chunk_indices: vec![1],
                },
            ],
            web_search_queries: vec!["hello world".to_string()],
        };
        let text = "Hello world! Goodbye world.";
        let annotated = annotate_with_citations(text, &meta);
        assert!(
            annotated.contains("[1]"),
            "should contain [1] marker: {annotated}"
        );
        assert!(
            annotated.contains("[2]"),
            "should contain [2] marker: {annotated}"
        );
        assert!(
            annotated.contains("References:"),
            "should contain References block: {annotated}"
        );
        assert!(
            annotated.contains("https://example.com/page1"),
            "should contain first URL: {annotated}"
        );
        assert!(
            annotated.contains("https://other.org/doc"),
            "should contain second URL: {annotated}"
        );
        assert!(
            annotated.contains("Example Page"),
            "should contain first title: {annotated}"
        );
    }

    #[test]
    fn annotate_with_citations_empty_chunks_returns_raw_text() {
        let meta = GroundingMetadata {
            grounding_chunks: vec![],
            grounding_supports: vec![GroundingSupport {
                segment: Some(GroundingSegment {
                    start_index: Some(0),
                    end_index: Some(5),
                }),
                grounding_chunk_indices: vec![0],
            }],
            web_search_queries: vec![],
        };
        let text = "Hello world";
        let result = annotate_with_citations(text, &meta);
        assert_eq!(result, text, "empty chunks must return raw text unchanged");
    }

    #[test]
    fn annotate_with_citations_empty_supports_returns_raw_text() {
        let meta = GroundingMetadata {
            grounding_chunks: vec![GroundingChunk {
                web: Some(GroundingChunkWeb {
                    uri: "https://example.com".to_string(),
                    title: None,
                }),
            }],
            grounding_supports: vec![],
            web_search_queries: vec![],
        };
        let text = "No citations here";
        let result = annotate_with_citations(text, &meta);
        assert_eq!(
            result, text,
            "empty supports must return raw text unchanged"
        );
    }

    #[test]
    fn annotate_with_citations_deduplicates_same_chunk_cited_multiple_times() {
        // Two supports both cite chunk 0 — chunk 0 should only appear as [1]
        // in the references list, not duplicated.
        let meta = GroundingMetadata {
            grounding_chunks: vec![GroundingChunk {
                web: Some(GroundingChunkWeb {
                    uri: "https://example.com".to_string(),
                    title: Some("Example".to_string()),
                }),
            }],
            grounding_supports: vec![
                GroundingSupport {
                    segment: Some(GroundingSegment {
                        start_index: Some(0),
                        end_index: Some(5),
                    }),
                    grounding_chunk_indices: vec![0],
                },
                GroundingSupport {
                    segment: Some(GroundingSegment {
                        start_index: Some(6),
                        end_index: Some(11),
                    }),
                    grounding_chunk_indices: vec![0],
                },
            ],
            web_search_queries: vec![],
        };
        let text = "Hello world";
        let result = annotate_with_citations(text, &meta);
        // [1] should appear as a marker twice (one per segment), but in the
        // References block the URL should appear only once.
        let url_count = result.matches("https://example.com").count();
        assert_eq!(
            url_count, 1,
            "URL should appear exactly once in References: {result}"
        );
        // Both segments should carry [1] markers
        assert!(result.contains("[1]"), "markers must be present: {result}");
        assert!(
            result.contains("References:"),
            "References block must be present: {result}"
        );
    }

    #[tokio::test]
    async fn grounding_metadata_in_stream_annotates_output_item_done() {
        // Simulates a full stream where the final chunk carries groundingMetadata.
        // NOTE: make_sse_bytes wraps each event as `data: <event>\n\n`, so the
        // JSON must be on a single line (no embedded newlines) to be parsed
        // correctly by the SSE parser.
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":"Rust 1.85 was released"}],"role":"model"}}]}"#,
            r#"{"candidates":[{"content":{"parts":[{"text":" with new features."}],"role":"model"},"finishReason":"STOP","groundingMetadata":{"groundingChunks":[{"web":{"uri":"https://blog.rust-lang.org/2025/02/20/Rust-1.85.0.html","title":"Rust 1.85.0 Release Blog"}}],"groundingSupports":[{"segment":{"startIndex":0,"endIndex":22},"groundingChunkIndices":[0],"confidenceScores":[0.95]}],"webSearchQueries":["Rust 1.85 release"]}}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":9,"totalTokenCount":19}}"#,
        ];
        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut collected = Vec::new();
        while let Some(event) = response.rx_event.recv().await {
            collected.push(event.unwrap());
        }

        // Find the OutputItemDone message.
        let done_item = collected.iter().find(|e| {
            matches!(
                e,
                ResponseEvent::OutputItemDone(ResponseItem::Message { .. })
            )
        });
        assert!(done_item.is_some(), "OutputItemDone must be emitted");

        if let ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) =
            done_item.unwrap()
        {
            match &content[0] {
                ContentItem::OutputText { text } => {
                    assert!(
                        text.contains("[1]"),
                        "citation marker [1] must be in text: {text}"
                    );
                    assert!(
                        text.contains("References:"),
                        "References block must be in text: {text}"
                    );
                    assert!(
                        text.contains("blog.rust-lang.org"),
                        "URL must be in References: {text}"
                    );
                }
                other => panic!("expected OutputText, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn thinking_text_with_signature_emits_reasoning_done() {
        let events = vec![
            r#"{"candidates":[{"content":{"parts":[{"text":"deep thought","thought":true,"thoughtSignature":"SIG_THINK"}],"role":"model"}}]}"#,
            r#"{"candidates":[{"content":{"parts":[{"text":"42"}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":3,"totalTokenCount":8}}"#,
        ];

        let stream = make_sse_bytes(&events);
        let mut response = spawn_generate_content_stream(stream, Duration::from_secs(30));

        let mut collected = Vec::new();
        while let Some(event) = response.rx_event.recv().await {
            collected.push(event.unwrap());
        }

        // Should have OutputItemDone(Reasoning) with raw_wire_block carrying the signature
        let done_reasoning = collected.iter().find(|e| {
            matches!(
                e,
                ResponseEvent::OutputItemDone(ResponseItem::Reasoning {
                    internal_chat_message_metadata_passthrough: None,

                    raw_wire_block: Some(_),
                    ..
                })
            )
        });
        assert!(
            done_reasoning.is_some(),
            "should emit OutputItemDone(Reasoning) with signature"
        );

        if let ResponseEvent::OutputItemDone(ResponseItem::Reasoning {
            internal_chat_message_metadata_passthrough: None,

            raw_wire_block: Some(block),
            ..
        }) = done_reasoning.unwrap()
        {
            assert_eq!(block["thoughtSignature"], "SIG_THINK");
            assert!(
                block.get("thought").is_none(),
                "thought field must be stripped from echo-back"
            );
        }
    }

    // GS-4: upstream_request_id plumb-through

    #[tokio::test]
    async fn upstream_request_id_extracted_from_x_request_id_header() {
        use codex_client::StreamResponse;
        use futures::stream;

        let mut headers = http::HeaderMap::new();
        headers.insert("x-request-id", "gemini-proxy-req-abc123".parse().unwrap());

        let rs = spawn_generate_content_stream_from_response(
            StreamResponse {
                status: http::StatusCode::OK,
                headers,
                bytes: Box::pin(stream::empty()),
            },
            Duration::from_secs(30),
        );

        assert_eq!(
            rs.upstream_request_id.as_deref(),
            Some("gemini-proxy-req-abc123"),
            "must extract upstream_request_id from x-request-id"
        );
    }

    #[tokio::test]
    async fn upstream_request_id_falls_back_to_goog_header() {
        use codex_client::StreamResponse;
        use futures::stream;

        let mut headers = http::HeaderMap::new();
        headers.insert(
            "x-goog-request-id",
            "goog-native-req-xyz789".parse().unwrap(),
        );

        let rs = spawn_generate_content_stream_from_response(
            StreamResponse {
                status: http::StatusCode::OK,
                headers,
                bytes: Box::pin(stream::empty()),
            },
            Duration::from_secs(30),
        );

        assert_eq!(
            rs.upstream_request_id.as_deref(),
            Some("goog-native-req-xyz789"),
            "must fall back to x-goog-request-id when x-request-id absent"
        );
    }

    #[tokio::test]
    async fn upstream_request_id_none_when_no_header_present() {
        use codex_client::StreamResponse;
        use futures::stream;

        let rs = spawn_generate_content_stream_from_response(
            StreamResponse {
                status: http::StatusCode::OK,
                headers: http::HeaderMap::new(),
                bytes: Box::pin(stream::empty()),
            },
            Duration::from_secs(30),
        );

        assert!(
            rs.upstream_request_id.is_none(),
            "upstream_request_id must be None when no header present"
        );
    }
}

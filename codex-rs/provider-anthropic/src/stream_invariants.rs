//! Stream invariant guards for the Anthropic `/v1/messages` SSE consumer.
//!
//! Ported from `refs/anthropic/anthropic-sdk-python/src/anthropic/lib/streaming/_messages.py`
//! (`accumulate_event` + helper, lines 362-499).
//!
//! Mirrors apex `streamInvariants.ts` (HCE-01a). Invariant IDs:
//! HI-C5-001..006, -010, HI-C1-004, -012.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Text,
    Thinking,
    RedactedThinking,
    ToolUse,
    ServerToolUse,
    Citations,
}

#[derive(Debug, Clone)]
pub struct BlockState {
    pub index: u32,
    pub kind: BlockKind,
    pub thinking_text: Option<String>,
    pub signature: Option<String>,
    pub input_json: Option<String>,
    pub data: Option<String>,
    pub signature_seen: bool,
    pub stopped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamInvariantViolation {
    UnknownEventType { event: String },
    UnknownDeltaSubtype { block_index: u32, delta_type: String },
    StopForUnknownIndex { block_index: u32 },
    DeltaForUnopenedIndex { block_index: u32, delta_type: String },
    SignatureBeforeThinking { block_index: u32 },
    DuplicateSignatureDelta { block_index: u32 },
    UsageMonotonicityViolation {
        field: &'static str,
        previous: u64,
        incoming: u64,
    },
    InStreamError { payload: String },
}

impl StreamInvariantViolation {
    /// Whether the violation should propagate as an error (true) or be
    /// logged + recovered (false).
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::StopForUnknownIndex { .. }
                | Self::DeltaForUnopenedIndex { .. }
                | Self::InStreamError { .. }
        )
    }
}

/// Guard a `content_block_delta` event before mutating accumulator state.
pub fn check_content_block_delta(
    block_index: u32,
    delta_type: &str,
    blocks: &BTreeMap<u32, BlockState>,
) -> Option<StreamInvariantViolation> {
    if !blocks.contains_key(&block_index) {
        return Some(StreamInvariantViolation::DeltaForUnopenedIndex {
            block_index,
            delta_type: delta_type.to_owned(),
        });
    }
    let _ = delta_type;
    None
}

/// Guard a `content_block_stop` event before mutating accumulator state.
pub fn check_content_block_stop(
    block_index: u32,
    blocks: &BTreeMap<u32, BlockState>,
) -> Option<StreamInvariantViolation> {
    if !blocks.contains_key(&block_index) {
        return Some(StreamInvariantViolation::StopForUnknownIndex { block_index });
    }
    None
}

/// Guard a `message_delta` usage update for monotonicity.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageSnapshot {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cache_creation_tokens: u64,
}

pub fn check_usage_monotonicity(
    incoming: UsageSnapshot,
    previous: UsageSnapshot,
) -> Option<StreamInvariantViolation> {
    if incoming.prompt_tokens > 0 && incoming.prompt_tokens < previous.prompt_tokens {
        return Some(StreamInvariantViolation::UsageMonotonicityViolation {
            field: "prompt_tokens",
            previous: previous.prompt_tokens,
            incoming: incoming.prompt_tokens,
        });
    }
    if incoming.completion_tokens > 0 && incoming.completion_tokens < previous.completion_tokens {
        return Some(StreamInvariantViolation::UsageMonotonicityViolation {
            field: "completion_tokens",
            previous: previous.completion_tokens,
            incoming: incoming.completion_tokens,
        });
    }
    None
}

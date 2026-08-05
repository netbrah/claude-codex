//! Unit tests for HCE-01b stream invariant guards (HI-C5 / HI-C1 matrix).

use codex_provider_anthropic::stream_accumulator::StreamAccumulator;
use codex_provider_anthropic::stream_accumulator::StreamEvent;
use codex_provider_anthropic::stream_accumulator::WireError;
use codex_provider_anthropic::stream_invariants::BlockKind;
use codex_provider_anthropic::stream_invariants::BlockState;
use codex_provider_anthropic::stream_invariants::StreamInvariantViolation;
use codex_provider_anthropic::stream_invariants::UsageSnapshot;
use codex_provider_anthropic::stream_invariants::check_content_block_delta;
use codex_provider_anthropic::stream_invariants::check_content_block_stop;
use codex_provider_anthropic::stream_invariants::check_usage_monotonicity;
use std::collections::BTreeMap;
use tracing_test::traced_test;

fn sample_block(index: u32) -> BlockState {
    BlockState {
        index,
        kind: BlockKind::Thinking,
        thinking_text: None,
        signature: None,
        input_json: None,
        data: None,
        signature_seen: false,
        stopped: false,
    }
}

#[test]
fn delta_for_unopened_index_is_fatal() {
    let blocks = BTreeMap::new();
    let v = check_content_block_delta(0, "thinking_delta", &blocks).unwrap();
    assert_eq!(
        v,
        StreamInvariantViolation::DeltaForUnopenedIndex {
            block_index: 0,
            delta_type: "thinking_delta".to_owned(),
        }
    );
    assert!(v.is_fatal());
}

#[test]
fn stop_for_unknown_index_is_fatal() {
    let blocks = BTreeMap::new();
    let v = check_content_block_stop(1, &blocks).unwrap();
    assert_eq!(
        v,
        StreamInvariantViolation::StopForUnknownIndex { block_index: 1 }
    );
    assert!(v.is_fatal());
}

#[test]
fn delta_for_opened_index_passes() {
    let mut blocks = BTreeMap::new();
    blocks.insert(0, sample_block(0));
    assert!(check_content_block_delta(0, "thinking_delta", &blocks).is_none());
}

#[test]
fn stop_for_opened_index_passes() {
    let mut blocks = BTreeMap::new();
    blocks.insert(0, sample_block(0));
    assert!(check_content_block_stop(0, &blocks).is_none());
}

#[test]
fn usage_non_monotonic_clamps() {
    let previous = UsageSnapshot {
        prompt_tokens: 100,
        completion_tokens: 50,
        cached_tokens: 0,
        cache_creation_tokens: 0,
    };
    let incoming = UsageSnapshot {
        prompt_tokens: 90,
        completion_tokens: 40,
        cached_tokens: 0,
        cache_creation_tokens: 0,
    };
    let v = check_usage_monotonicity(incoming, previous).unwrap();
    assert!(matches!(
        v,
        StreamInvariantViolation::UsageMonotonicityViolation {
            field: "prompt_tokens",
            ..
        }
    ));
    assert!(!v.is_fatal());
}

#[test]
fn usage_monotonic_update_passes() {
    let previous = UsageSnapshot {
        prompt_tokens: 100,
        completion_tokens: 50,
        cached_tokens: 0,
        cache_creation_tokens: 0,
    };
    let incoming = UsageSnapshot {
        prompt_tokens: 110,
        completion_tokens: 60,
        cached_tokens: 0,
        cache_creation_tokens: 0,
    };
    assert!(check_usage_monotonicity(incoming, previous).is_none());
}

#[test]
fn usage_zero_incoming_skips_monotonicity_check() {
    let previous = UsageSnapshot {
        prompt_tokens: 100,
        completion_tokens: 50,
        cached_tokens: 0,
        cache_creation_tokens: 0,
    };
    let incoming = UsageSnapshot::default();
    assert!(check_usage_monotonicity(incoming, previous).is_none());
}

#[test]
fn in_stream_error_is_fatal() {
    let v = StreamInvariantViolation::InStreamError {
        payload: "rate_limit".to_owned(),
    };
    assert!(v.is_fatal());
}

#[test]
fn signature_before_thinking_is_warn_only() {
    let v = StreamInvariantViolation::SignatureBeforeThinking { block_index: 0 };
    assert!(!v.is_fatal());
}

#[test]
fn duplicate_signature_delta_is_warn_only() {
    let v = StreamInvariantViolation::DuplicateSignatureDelta { block_index: 0 };
    assert!(!v.is_fatal());
}

// ── StreamAccumulator integration (commits 5b–5c) ─────────────────

#[test]
fn ping_event_is_noop() {
    let mut acc = StreamAccumulator::new();
    let before = acc.blocks().len();
    acc.accumulate_event(StreamEvent::Ping).unwrap();
    assert_eq!(acc.blocks().len(), before);
}

#[test]
fn error_event_propagates_typed() {
    let mut acc = StreamAccumulator::new();
    let err = acc
        .accumulate_event(StreamEvent::Error {
            payload: "overloaded".to_owned(),
        })
        .unwrap_err();
    assert_eq!(
        err,
        WireError::InStreamError {
            payload: "overloaded".to_owned()
        }
    );
}

#[traced_test]
#[test]
fn unknown_event_type_warns() {
    let mut acc = StreamAccumulator::new();
    acc.accumulate_event(StreamEvent::Unknown {
        tag: "future_event".to_owned(),
    })
    .unwrap();
    assert!(logs_contain("anthropic-stream:unknown-event"));
}

#[test]
fn accumulator_stop_for_unknown_index_is_fatal() {
    let mut acc = StreamAccumulator::new();
    let err = acc
        .accumulate_event(StreamEvent::ContentBlockStop { index: 9 })
        .unwrap_err();
    assert!(matches!(
        err,
        WireError::StreamStateCorruption(StreamInvariantViolation::StopForUnknownIndex {
            block_index: 9
        })
    ));
}

#[test]
fn accumulator_delta_for_unopened_index_is_fatal() {
    let mut acc = StreamAccumulator::new();
    let err = acc
        .accumulate_event(StreamEvent::ContentBlockDelta {
            index: 0,
            delta_type: "thinking_delta".to_owned(),
            text: None,
            thinking: Some("x".to_owned()),
            signature: None,
            partial_json: None,
        })
        .unwrap_err();
    assert!(matches!(
        err,
        WireError::StreamStateCorruption(StreamInvariantViolation::DeltaForUnopenedIndex { .. })
    ));
}

#[traced_test]
#[test]
fn unknown_delta_subtype_warns() {
    let mut acc = StreamAccumulator::new();
    acc.accumulate_event(StreamEvent::ContentBlockStart {
        index: 0,
        kind: BlockKind::Thinking,
    })
    .unwrap();
    acc.accumulate_event(StreamEvent::ContentBlockDelta {
        index: 0,
        delta_type: "future_delta".to_owned(),
        text: None,
        thinking: None,
        signature: None,
        partial_json: None,
    })
    .unwrap();
    assert!(logs_contain("anthropic-stream:unknown-delta-subtype"));
}

#[test]
fn accumulator_usage_non_monotonic_clamps() {
    let mut acc = StreamAccumulator::new();
    acc.accumulate_event(StreamEvent::MessageDelta {
        usage: UsageSnapshot {
            prompt_tokens: 100,
            completion_tokens: 50,
            cached_tokens: 0,
            cache_creation_tokens: 0,
        },
    })
    .unwrap();
    acc.accumulate_event(StreamEvent::MessageDelta {
        usage: UsageSnapshot {
            prompt_tokens: 90,
            completion_tokens: 40,
            cached_tokens: 0,
            cache_creation_tokens: 0,
        },
    })
    .unwrap();
    assert_eq!(
        acc.usage(),
        UsageSnapshot {
            prompt_tokens: 100,
            completion_tokens: 50,
            cached_tokens: 0,
            cache_creation_tokens: 0,
        }
    );
}

#[traced_test]
#[test]
fn signature_before_thinking_warns() {
    let mut acc = StreamAccumulator::new();
    acc.accumulate_event(StreamEvent::ContentBlockStart {
        index: 0,
        kind: BlockKind::Thinking,
    })
    .unwrap();
    acc.accumulate_event(StreamEvent::ContentBlockDelta {
        index: 0,
        delta_type: "signature_delta".to_owned(),
        text: None,
        thinking: None,
        signature: Some("sig1".to_owned()),
        partial_json: None,
    })
    .unwrap();
    assert!(logs_contain("anthropic-stream:signature-before-thinking"));
}

#[traced_test]
#[test]
fn duplicate_signature_delta_warns_and_concats() {
    let mut acc = StreamAccumulator::new();
    acc.accumulate_event(StreamEvent::ContentBlockStart {
        index: 0,
        kind: BlockKind::Thinking,
    })
    .unwrap();
    for sig in ["sig1", "sig2"] {
        acc.accumulate_event(StreamEvent::ContentBlockDelta {
            index: 0,
            delta_type: "signature_delta".to_owned(),
            text: None,
            thinking: None,
            signature: Some(sig.to_owned()),
            partial_json: None,
        })
        .unwrap();
    }
    assert!(logs_contain("anthropic-stream:duplicate-signature-delta"));
    assert_eq!(
        acc.blocks().get(&0).unwrap().signature.as_deref(),
        Some("sig1sig2")
    );
}

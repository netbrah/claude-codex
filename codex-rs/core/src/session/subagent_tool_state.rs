//! Per-owner `tool_use_id` lifecycle tracking for subagent fold-in.
//!
//! Ported from `refs/anthropic/anthropic-sdk-python/src/anthropic/lib/tools/_beta_session_runner.py`
//! lines 393-397, 459-502, 584-597.

use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::Deref;

use codex_protocol::models::ResponseItem;

/// Identifier of the parent tool call that delegated to a subagent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ParentCallId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ToolUseId(pub String);

#[derive(Debug, Default, Clone)]
pub struct SubagentToolState {
    answered: HashMap<ParentCallId, HashSet<ToolUseId>>,
    seen: HashSet<ToolUseId>,
    counters: Counters,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    pub answered: u64,
    pub seen_only: u64,
    pub unowned_skipped: u64,
    pub cascade_stripped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkResult {
    Answered { owner: ParentCallId },
    UnownedSkipped { reason: UnownedReason },
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnownedReason {
    NoOwnerContext,
    ForeignOwner,
}

impl SubagentToolState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn witness(&mut self, id: ToolUseId) {
        if self.seen.insert(id) {
            self.counters.seen_only = self.counters.seen_only.saturating_add(1);
        }
    }

    pub fn mark_answered(&mut self, id: ToolUseId, owner: Option<ParentCallId>) -> MarkResult {
        let Some(owner) = owner else {
            self.counters.unowned_skipped = self.counters.unowned_skipped.saturating_add(1);
            tracing::debug!(
                tool_use_id = %id.0,
                reason = "no-owner-context",
                "subagent:tool-reconciliation"
            );
            return MarkResult::UnownedSkipped {
                reason: UnownedReason::NoOwnerContext,
            };
        };

        let owned = self.answered.entry(owner.clone()).or_default();
        if !owned.insert(id.clone()) {
            return MarkResult::Duplicate;
        }
        self.counters.answered = self.counters.answered.saturating_add(1);
        if self.counters.seen_only > 0 {
            self.counters.seen_only = self.counters.seen_only.saturating_sub(1);
        }
        MarkResult::Answered { owner }
    }

    pub fn is_answered(&self, id: &ToolUseId) -> bool {
        self.answered.values().any(|set| set.contains(id))
    }

    pub fn is_seen(&self, id: &ToolUseId) -> bool {
        self.seen.contains(id)
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    pub fn owned_ids<'a>(
        &self,
        ids: impl IntoIterator<Item = &'a ToolUseId>,
        owner: &ParentCallId,
    ) -> HashSet<ToolUseId> {
        let Some(owned) = self.answered.get(owner) else {
            return HashSet::new();
        };
        ids.into_iter()
            .filter(|id| owned.contains(*id))
            .cloned()
            .collect()
    }

    pub fn record_cascade_strip(&mut self, _id: &ToolUseId) {
        self.counters.cascade_stripped = self.counters.cascade_stripped.saturating_add(1);
        tracing::debug!(event = "cascade-strip", "subagent:tool-reconciliation");
    }

    pub fn track_response_item(&mut self, item: &ResponseItem, owner: Option<ParentCallId>) {
        match item {
            ResponseItem::FunctionCall { call_id, .. } => {
                self.witness(ToolUseId(call_id.clone()));
            }
            ResponseItem::FunctionCallOutput { call_id, .. } => {
                let _ = self.mark_answered(ToolUseId(call_id.clone()), owner);
            }
            _ => {}
        }
    }

    pub fn track_response_items<I>(&mut self, items: I, owner: Option<ParentCallId>)
    where
        I: IntoIterator,
        I::Item: std::ops::Deref<Target = ResponseItem>,
    {
        for item in items {
            self.track_response_item(item.deref(), owner.clone());
        }
    }
}

/// Rebuild answered/seen sets from compacted or resumed history.
pub fn rehydrate_subagent_tool_state(history: &[ResponseItem]) -> SubagentToolState {
    let mut state = SubagentToolState::new();
    for item in history {
        state.track_response_item(item, None);
    }
    state
}

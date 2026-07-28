use codex_core::subagent_tool_state::{
    MarkResult, ParentCallId, SubagentToolState, ToolUseId, UnownedReason,
    rehydrate_subagent_tool_state,
};
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use tracing_test::traced_test;

fn id(s: &str) -> ToolUseId {
    ToolUseId(s.to_string())
}

fn owner(s: &str) -> ParentCallId {
    ParentCallId(s.to_string())
}

fn function_call(call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: "test_tool".to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: call_id.to_string(),
    }
}

fn function_call_output(call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload::from_text("ok".to_string()),
    }
}

#[test]
fn witness_then_mark_increments_answered() {
    let mut state = SubagentToolState::new();
    state.witness(id("tool_1"));
    assert_eq!(state.counters().seen_only, 1);

    let result = state.mark_answered(id("tool_1"), Some(owner("parent_spawn")));
    assert!(matches!(result, MarkResult::Answered { .. }));
    assert_eq!(state.counters().answered, 1);
    assert_eq!(state.counters().seen_only, 0);
}

#[traced_test]
#[test]
fn mark_answered_without_owner_returns_unowned_skipped() {
    let mut state = SubagentToolState::new();
    state.witness(id("abc"));
    let result = state.mark_answered(id("abc"), None);
    assert!(matches!(
        result,
        MarkResult::UnownedSkipped {
            reason: UnownedReason::NoOwnerContext
        }
    ));
    assert!(!state.is_answered(&id("abc")));
    assert_eq!(state.counters().unowned_skipped, 1);
    assert!(logs_contain("subagent:tool-reconciliation"));
    assert!(logs_contain("no-owner-context"));
}

#[traced_test]
#[test]
fn mark_answered_with_unknown_owner_treats_as_unowned() {
    let mut state = SubagentToolState::new();
    state.witness(id("foreign_tool"));
    let result = state.mark_answered(id("foreign_tool"), None);
    assert!(matches!(
        result,
        MarkResult::UnownedSkipped {
            reason: UnownedReason::NoOwnerContext
        }
    ));
    assert!(!state.is_answered(&id("foreign_tool")));
}

#[test]
fn is_answered_true_only_after_mark() {
    let mut state = SubagentToolState::new();
    state.witness(id("pending"));
    assert!(!state.is_answered(&id("pending")));

    state.mark_answered(id("pending"), Some(owner("parent")));
    assert!(state.is_answered(&id("pending")));
}

#[test]
fn parent_and_subagent_share_id_distinct_owners_no_mispair() {
    let mut state = SubagentToolState::new();
    let shared = id("tool_42");
    let parent = owner("parent_spawn");
    let subagent = owner("subagent_spawn");

    state.witness(shared.clone());
    assert!(
        state
            .mark_answered(shared.clone(), Some(parent.clone()))
            .is_answered()
    );

    state.witness(shared.clone());
    assert!(
        state
            .mark_answered(shared.clone(), Some(subagent.clone()))
            .is_answered()
    );

    let parent_owned = state.owned_ids([&shared], &parent);
    let subagent_owned = state.owned_ids([&shared], &subagent);
    assert_eq!(parent_owned.len(), 1);
    assert_eq!(subagent_owned.len(), 1);
    assert!(parent_owned.contains(&shared));
    assert!(subagent_owned.contains(&shared));
}

#[test]
fn owned_ids_filters_foreign_subagent_ids() {
    let mut state = SubagentToolState::new();
    let parent_tool = id("tool_parent");
    let subagent_tool = id("tool_sub");
    let parent = owner("parent_spawn");
    let subagent = owner("subagent_spawn");

    state.witness(parent_tool.clone());
    state.witness(subagent_tool.clone());
    state.mark_answered(parent_tool.clone(), Some(parent.clone()));
    state.mark_answered(subagent_tool.clone(), Some(subagent));

    let filtered = state.owned_ids([&parent_tool, &subagent_tool], &parent);
    assert_eq!(filtered.len(), 1);
    assert!(filtered.contains(&parent_tool));
    assert!(!filtered.contains(&subagent_tool));
}

#[test]
fn duplicate_mark_returns_duplicate_not_double_count() {
    let mut state = SubagentToolState::new();
    let tool = id("dup");
    let parent = owner("parent");

    state.witness(tool.clone());
    assert!(matches!(
        state.mark_answered(tool.clone(), Some(parent.clone())),
        MarkResult::Answered { .. }
    ));
    assert!(matches!(
        state.mark_answered(tool.clone(), Some(parent)),
        MarkResult::Duplicate
    ));
    assert_eq!(state.counters().answered, 1);
}

#[test]
fn rehydrate_from_history_populates_seen_and_answered() {
    let history = vec![function_call("t1"), function_call_output("t1")];
    let state = rehydrate_subagent_tool_state(&history);
    assert!(state.is_seen(&id("t1")));
    // Owner not plumbed on rehydrate; output stays pending per Python SDK rule.
    assert!(!state.is_answered(&id("t1")));
    assert_eq!(state.counters().unowned_skipped, 1);

    let mut with_owner = SubagentToolState::new();
    with_owner.track_response_item(&function_call("t2"), None);
    with_owner.track_response_item(&function_call_output("t2"), Some(owner("parent")));
    assert!(with_owner.is_seen(&id("t2")));
    assert!(with_owner.is_answered(&id("t2")));
}

trait MarkResultExt {
    fn is_answered(&self) -> bool;
}

impl MarkResultExt for MarkResult {
    fn is_answered(&self) -> bool {
        matches!(self, MarkResult::Answered { .. })
    }
}

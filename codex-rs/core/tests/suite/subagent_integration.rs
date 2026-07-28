//! HCE-02b integration: subagent tool_use_id collision + fold-in invariants.

use codex_core::subagent_tool_state::{ParentCallId, SubagentToolState, ToolUseId};

fn id(s: &str) -> ToolUseId {
    ToolUseId(s.to_string())
}

fn owner(s: &str) -> ParentCallId {
    ParentCallId(s.to_string())
}

/// Parent and subagent trajectories use separate `SubagentToolState` instances
/// (namespace isolation). A shared `tool_use_id` string must not cross-pollute.
#[test]
fn collision_regression_parent_subagent_shared_tool_use_id() {
    let mut parent_state = SubagentToolState::new();
    let mut subagent_state = SubagentToolState::new();
    let shared = id("tool_42");

    parent_state.witness(shared.clone());
    parent_state.mark_answered(shared.clone(), Some(owner("parent_spawn_call")));

    subagent_state.witness(shared.clone());
    subagent_state.mark_answered(shared.clone(), Some(owner("subagent_spawn_call")));

    let parent_owned = parent_state.owned_ids([&shared], &owner("parent_spawn_call"));
    let subagent_owned = subagent_state.owned_ids([&shared], &owner("subagent_spawn_call"));

    assert_eq!(parent_owned.len(), 1);
    assert_eq!(subagent_owned.len(), 1);
    assert!(parent_state.is_answered(&shared));
    assert!(subagent_state.is_answered(&shared));
}

/// Fold-in merge must filter foreign ids via `owned_ids` so orphan cleanup
/// does not cascade-strip the other trajectory's tool calls.
#[test]
fn fold_in_invariance_owned_ids_filters_foreign() {
    let mut parent_state = SubagentToolState::new();
    let parent_tool = id("tool_parent");
    let subagent_tool = id("tool_sub");
    let parent_owner = owner("parent_spawn");

    parent_state.witness(parent_tool.clone());
    parent_state.witness(subagent_tool.clone());
    parent_state.mark_answered(parent_tool.clone(), Some(parent_owner.clone()));
    // Subagent tool witnessed in parent history but answered only in subagent namespace.
    parent_state.mark_answered(subagent_tool.clone(), None);

    let fold_in_candidates = [parent_tool.clone(), subagent_tool.clone()];
    let owned_for_parent = parent_state.owned_ids(fold_in_candidates.iter(), &parent_owner);

    assert_eq!(owned_for_parent.len(), 1);
    assert!(owned_for_parent.contains(&parent_tool));
    assert!(!owned_for_parent.contains(&subagent_tool));
    assert!(!parent_state.is_answered(&subagent_tool));
}

/// HI-C2-004 activation: per-owner `mark_answered` increments `answered` when
/// `ParentCallId` is supplied (fork-context spawn plumbing).
#[test]
fn subagent_owner_tracking_activates_when_spawned_with_fork_context() {
    let mut state = SubagentToolState::new();
    let tool_id = id("tool_fork_ctx");
    let parent_owner = owner("parent_call_xyz");

    state.witness(tool_id.clone());
    let result = state.mark_answered(tool_id.clone(), Some(parent_owner.clone()));

    assert!(matches!(result, codex_core::subagent_tool_state::MarkResult::Answered { .. }));
    assert_eq!(state.counters().answered, 1);
    assert_eq!(state.counters().unowned_skipped, 0);
    assert!(state.is_answered(&tool_id));
}

/// Documents global-mode fallback when `parent_spawn_call_id` is absent on ThreadSpawn.
#[test]
fn subagent_owner_tracking_falls_back_to_global_when_no_fork_context() {
    let mut state = SubagentToolState::new();
    let tool_id = id("tool_no_fork");

    state.witness(tool_id.clone());
    let result = state.mark_answered(tool_id.clone(), None);

    assert!(matches!(
        result,
        codex_core::subagent_tool_state::MarkResult::UnownedSkipped { .. }
    ));
    assert_eq!(state.counters().answered, 0);
    assert_eq!(state.counters().unowned_skipped, 1);
    assert!(!state.is_answered(&tool_id));
}

#[test]
fn session_bundle_pre_owner_plumbing_deserializes_with_none() {
    use codex_protocol::protocol::SubAgentSource;

    let raw = include_str!("../fixtures/session_bundle_pre_owner_plumbing.json");
    let source: SubAgentSource = serde_json::from_str(raw).expect("deserialize");
    match source {
        SubAgentSource::ThreadSpawn {
            parent_spawn_call_id,
            ..
        } => assert_eq!(parent_spawn_call_id, None),
        other => panic!("expected ThreadSpawn, got {other:?}"),
    }
}

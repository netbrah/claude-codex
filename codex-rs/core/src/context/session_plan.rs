use codex_protocol::plan_tool::{PlanItemArg, StepStatus, UpdatePlanArgs};
use serde::Deserialize;
use serde::Serialize;

/// Snapshot of the current plan, stored in SessionState and re-injected each turn.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanState {
    pub items: Vec<PlanItemArg>,
}

/// Threshold above which completed items are compacted to a count summary.
const COMPLETED_COMPACT_THRESHOLD: usize = 5;

impl PlanState {
    /// Build a `PlanState` from the latest `update_plan` call arguments.
    pub fn from_update(args: &UpdatePlanArgs) -> Self {
        Self {
            items: args.plan.clone(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Render plan state for context injection. Compacts completed items when
    /// there are more than [`COMPLETED_COMPACT_THRESHOLD`].
    pub fn to_context_string(&self) -> String {
        let completed: Vec<_> = self
            .items
            .iter()
            .filter(|i| matches!(i.status, StepStatus::Completed))
            .collect();
        let active: Vec<_> = self
            .items
            .iter()
            .filter(|i| !matches!(i.status, StepStatus::Completed))
            .collect();

        let mut lines = Vec::new();

        if completed.len() > COMPLETED_COMPACT_THRESHOLD {
            lines.push(format!("({} tasks completed)", completed.len()));
        } else {
            for item in &completed {
                lines.push(format!("- [completed] {}", item.step));
            }
        }

        for item in &active {
            let status = match item.status {
                StepStatus::Pending => "pending",
                StepStatus::InProgress => "in_progress",
                StepStatus::Completed => "completed",
            };
            lines.push(format!("- [{}] {}", status, item.step));
        }

        lines.join("\n")
    }

    /// Returns the description of the currently in-progress task, if any.
    // XLI utility for context injection; production callers live in TUI/exec
    // (apex). Kept here as the canonical fork-only helper alongside its tests.
    #[allow(dead_code)]
    pub fn in_progress_task(&self) -> Option<&str> {
        self.items
            .iter()
            .find(|i| matches!(i.status, StepStatus::InProgress))
            .map(|i| i.step.as_str())
    }
}

#[cfg(test)]
mod plan_state_tests {
    use super::*;
    use codex_protocol::plan_tool::PlanItemArg;
    use codex_protocol::plan_tool::StepStatus;
    use codex_protocol::plan_tool::UpdatePlanArgs;

    fn item(step: &str, status: StepStatus) -> PlanItemArg {
        PlanItemArg {
            step: step.to_string(),
            status,
        }
    }

    #[test]
    fn empty_plan_state() {
        let state = PlanState::default();
        assert!(state.is_empty());
        assert_eq!(state.to_context_string(), "");
        assert_eq!(state.in_progress_task(), None);
    }

    #[test]
    fn basic_rendering() {
        let state = PlanState {
            items: vec![
                item("Fix segfault", StepStatus::Completed),
                item("Refactor parser", StepStatus::InProgress),
                item("Add tests", StepStatus::Pending),
            ],
        };
        let rendered = state.to_context_string();
        assert!(rendered.contains("- [completed] Fix segfault"));
        assert!(rendered.contains("- [in_progress] Refactor parser"));
        assert!(rendered.contains("- [pending] Add tests"));
    }

    #[test]
    fn compacts_many_completed() {
        let mut items: Vec<PlanItemArg> = (0..7)
            .map(|i| item(&format!("Done {i}"), StepStatus::Completed))
            .collect();
        items.push(item("Active task", StepStatus::InProgress));
        items.push(item("Future task", StepStatus::Pending));

        let state = PlanState { items };
        let rendered = state.to_context_string();
        assert!(rendered.contains("(7 tasks completed)"));
        assert!(!rendered.contains("- [completed]"));
        assert!(rendered.contains("- [in_progress] Active task"));
        assert!(rendered.contains("- [pending] Future task"));
    }

    #[test]
    fn does_not_compact_few_completed() {
        let items: Vec<PlanItemArg> = (0..4)
            .map(|i| item(&format!("Done {i}"), StepStatus::Completed))
            .collect();
        let state = PlanState { items };
        let rendered = state.to_context_string();
        assert!(rendered.contains("- [completed] Done 0"));
        assert!(rendered.contains("- [completed] Done 3"));
        assert!(!rendered.contains("tasks completed)"));
    }

    #[test]
    fn in_progress_task_returns_correct() {
        let state = PlanState {
            items: vec![
                item("Done", StepStatus::Completed),
                item("Working on this", StepStatus::InProgress),
                item("Later", StepStatus::Pending),
            ],
        };
        assert_eq!(state.in_progress_task(), Some("Working on this"));
    }

    #[test]
    fn in_progress_task_none_when_all_done() {
        let state = PlanState {
            items: vec![
                item("Done 1", StepStatus::Completed),
                item("Done 2", StepStatus::Completed),
            ],
        };
        assert_eq!(state.in_progress_task(), None);
    }

    #[test]
    fn from_update_copies_items() {
        let args = UpdatePlanArgs {
            explanation: Some("starting".to_string()),
            plan: vec![
                item("Step A", StepStatus::Pending),
                item("Step B", StepStatus::Pending),
            ],
        };
        let state = PlanState::from_update(&args);
        assert_eq!(state.items.len(), 2);
        assert_eq!(state.items[0].step, "Step A");
    }
}

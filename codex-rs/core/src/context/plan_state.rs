use super::ContextualUserFragment;

/// Contextual user fragment that re-injects the latest `update_plan` state
/// each turn so the model always sees its current task list, surviving
/// compaction and interruptions.
pub(crate) struct PlanStateFragment {
    body: String,
}

impl PlanStateFragment {
    pub(crate) fn new(body: impl Into<String>) -> Self {
        Self { body: body.into() }
    }
}

impl ContextualUserFragment for PlanStateFragment {
    fn role() -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<plan_state>", "</plan_state>")
    }

    fn body(&self) -> String {
        self.body.clone()
    }
}

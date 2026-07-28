# XLI 70/30 Split Wiring — Clean Implementation Prompt

## Context

The structured XML compact prompt and `find_compact_split_point()` function are already landed and tested. The remaining work is to **wire the split into `run_compact_task_inner`** to actually preserve the recent 30% of history verbatim after compaction.

A previous attempt at this was reverted because it broke 12 integration tests. This prompt provides a precise, test-by-test analysis of what breaks and why, along with the exact code changes needed.

## What Already Exists (DO NOT MODIFY)

| Symbol | Location | Status |
|--------|----------|--------|
| `PRESERVE_FRACTION: f64 = 0.3` | `compact.rs:37` | Landed |
| `find_compact_split_point(items)` | `compact.rs:246-266` | Landed + tested |
| `find_next_user_message_boundary()` | `compact.rs:268-280` | Landed |
| `is_tool_response_content()` | `compact.rs:282-284` | Landed |
| Structured XML prompt | `templates/compact/prompt.md` | Landed |
| Split-point unit tests (4 tests) | `compact_tests.rs:563-695` | Landed |

## The Single Code Change

**File:** `codex-rs/core/src/compact.rs`
**Function:** `run_compact_task_inner()` (line 94)
**Lines to replace:** 197-203 (the post-compact assembly block)

### Current code (lines 197-203):

```rust
let history_snapshot = sess.clone_history().await;
let history_items = history_snapshot.raw_items();
let summary_suffix = get_last_assistant_message_from_turn(history_items).unwrap_or_default();
let summary_text = format!("{SUMMARY_PREFIX}\n{summary_suffix}");
let user_messages = collect_user_messages(history_items);

let mut new_history = build_compacted_history(Vec::new(), &user_messages, &summary_text);
```

### New code:

```rust
let history_snapshot = sess.clone_history().await;
let history_items = history_snapshot.raw_items();
let summary_suffix = get_last_assistant_message_from_turn(history_items).unwrap_or_default();
let summary_text = format!("{SUMMARY_PREFIX}\n{summary_suffix}");

// --- 70/30 split: use PRE-COMPACT items for the split calculation ---
// pre_compact_items was captured at line 107, BEFORE the compact prompt
// and LLM summary were appended. Using history_items (post-compact snapshot)
// would corrupt the split because it includes the compact prompt + LLM response.
let split_point = find_compact_split_point(&pre_compact_items);
let preserved_items: Vec<ResponseItem> = pre_compact_items[split_point..].to_vec();

let mut new_history: Vec<ResponseItem> = Vec::new();

// 1. Summary as user message
new_history.push(ResponseItem::Message {
    id: None,
    role: "user".to_string(),
    content: vec![ContentItem::InputText {
        text: summary_text.clone(),
    }],
    end_turn: None,
    phase: None,
});

// 2. Preserved recent history (verbatim, including tool calls, assistant msgs, reasoning)
new_history.extend(preserved_items);
```

### Critical details:

1. **Use `pre_compact_items` (line 107), NOT `history_snapshot.raw_items()`** for the split.
   `pre_compact_items` is the history before the compact prompt was appended. The post-compact
   `history_snapshot` includes the compact prompt user message AND the LLM's summary response,
   which would pollute the split-point calculation.

2. **Remove the `#[allow(unused_variables)]` annotation** on `pre_compact_items` (line 106)
   since it is now used.

3. **No synthetic model ack message.** The previous attempt added an assistant "Got it" message
   between summary and preserved items. This is unnecessary and complicates the history shape.
   The preserved items already maintain correct conversation alternation because
   `find_compact_split_point` lands on a user message boundary.

4. **No changes to anything after line 203.** The `InitialContextInjection`, ghost_snapshots,
   `replace_compacted_history` call, and everything else stays exactly as-is.

5. **`build_compacted_history` and `collect_user_messages` remain in the codebase.** They are
   still called from `codex.rs:6162` (connector mention extraction) and
   `rollout_reconstruction.rs:265` (legacy rollout replay). Do not remove them.

## New Post-Compact History Shape

### Before (current):

```
[env_context, user_msg_1, user_msg_2, ..., summary_text]
```

All user messages collected, summary appended as the last item.

### After (new):

```
[summary_text, ...preserved_recent_items]
```

Summary as first item, then the recent ~30% of pre-compact history verbatim.
The preserved portion starts at a user message boundary (guaranteed by
`find_compact_split_point`), so it naturally begins with a user message.

### What `InitialContextInjection::BeforeLastUserMessage` does with this shape:

The `insert_initial_context_before_last_real_user_or_summary` function scans backward
for the last real user message. In the new shape, the preserved portion contains user
messages (it starts at one), so the function finds the last real user message in the
preserved tail and inserts initial context before it. This works correctly.

**Edge case:** If `find_compact_split_point` returns `items.len()` (no user message
found in the 30% tail), the preserved portion is empty and the history is just
`[summary_text]`. The insertion function then treats the summary as a
"user_or_summary" item and inserts context before it -- same as today. OK.

## The 12 Failing Tests -- Exhaustive Analysis

The tests fail because they assert on the post-compact history shape. The new shape
replaces `[user_messages..., summary]` with `[summary, ...preserved_items]`.

### Category A: Snapshot tests (7 tests) -- re-record after review

These tests use `insta::assert_snapshot!` to capture the full request shape.
The snapshots will show the new history layout. Run the tests, review the new
snapshots, and accept them.

| # | Test | File | Snapshot |
|---|------|------|----------|
| 1 | `snapshot_request_shape_mid_turn_continuation_compaction` | `compact.rs:2599` | `mid_turn_compaction_shapes` |
| 2 | `snapshot_request_shape_manual_compact_without_previous_user_messages` | `compact.rs:3305` | `manual_compact_without_prev_user_shapes` |
| 3 | `snapshot_request_shape_pre_turn_compaction_including_incoming_user_message` | `compact.rs:2971` | `pre_turn_compaction_including_incoming_shapes` |
| 4 | `snapshot_request_shape_pre_turn_compaction_strips_incoming_model_switch` | `compact.rs:3091` | `pre_turn_compaction_strips_incoming_model_switch_shapes` |
| 5 | `snapshot_request_shape_pre_turn_compaction_context_window_exceeded` | `compact.rs:3218` | `pre_turn_compaction_context_window_exceeded_shapes` |
| 6 | `snapshot_rollback_past_compaction_replays_append_only_history` | `compact_resume_fork.rs:445` | `rollback_past_compaction_shapes` |
| 7 | `manual_compact_twice_preserves_latest_user_messages` | `compact.rs:2289` | `manual_compact_with_history_shapes` |

**Action:** Run tests, review `.snap.new` files, then `cargo insta accept -p codex-core`

### Category B: Assertion-based tests (5 tests) -- update assertions

These tests make explicit assertions about post-compact history content.
Each one needs specific assertion changes documented below.

---

#### B1: `summarize_context_three_requests_and_instructions` (compact.rs:202)

**What it tests:** Manual compact flow -- 3 requests: normal turn, compact, follow-up.

**Assertions that break (request 3 -- post-compact follow-up):**

1. `assert_eq!(assistant_count, 0, "assistant history should be cleared");`
   - Breaks because preserved 30% portion may include assistant messages.
   - Fix: Remove this assertion. The new shape intentionally preserves assistant messages.

2. `assert!(messages.iter().any(|(r, t)| r == "user" && t == "hello world"));`
   - May break: "hello world" might be in summarized 70% or preserved 30% depending on history size.
   - In this test (1 user msg + 1 assistant reply), the history is very small.
     `find_compact_split_point` will likely put everything in preserved portion OR summarize
     everything (if no user message is found in the 30% tail).
   - Fix: Test which case applies; if "hello world" gets summarized, remove this assertion.

3. Summary and no-summarization-prompt assertions: Still pass -- no change needed.

---

#### B2: `multiple_auto_compact_per_task_runs_after_token_limit_hit` (compact.rs:633)

**What it tests:** Three rounds of auto-compact during a single turn with tool calls.

**Assertions that break:**
- `assert_eq!(input.len(), 3)` -- post-compact input is no longer exactly 3 items.
- The 400-line `expected_requests_inputs` JSON blob specifies exact shapes for all 7 requests.
  Post-compact requests (indices 2, 4, 6) need updating.

**Key insight:** In this test, each work chunk produces [reasoning, local_shell_call, fco].
There are NO user messages in the work chunks. The only user message is "create an app" at
the beginning. So after the 70% mark, `find_next_user_message_boundary` will NOT find
a user message and will return `items.len()`, meaning **preserved_items is EMPTY**.
The new post-compact history will be just `[summary]`.

This means the post-compact requests (indices 2, 4, 6) change from:
`[env_context, user_msg("create an app"), summary]` to just `[summary]`.

But wait -- the initial context re-injection then adds `env_context` and seeded prefix
back via `InitialContextInjection::DoNotInject` path (manual compact uses DoNotInject,
and auto-compact mid-turn uses BeforeLastUserMessage). Check which path applies here.

For auto-compact: `run_inline_auto_compact_task` passes `initial_context_injection`
from the caller. Mid-turn auto-compact uses `BeforeLastUserMessage`.

So with `BeforeLastUserMessage` and no user messages in preserved portion:
- `insert_initial_context_before_last_real_user_or_summary` finds summary as "last_user_or_summary"
- Inserts initial context BEFORE the summary
- Result: `[initial_context..., summary]`

Then the NEXT turn re-injects `reference_context_item` which adds env_context + seeded prefix.

**Strategy:** Run the test, inspect actual output, update the JSON blob accordingly.
Consider restructuring assertions to check for presence of key elements rather than
exact array equality -- this makes the test resilient to future shape changes.

---

#### B3: `pre_sampling_compact_runs_on_switch_to_smaller_context_model` (compact.rs:1696)

**What it tests:** Model switch triggers pre-sampling compaction.

**Likely break:** The `assert_pre_sampling_switch_compaction_requests` helper (line 116)
and follow-up assertions check post-compact request shape.

**Fix:** Update expected shapes in the helper. Core behavior (compact before model switch,
compact strips model-switch items) is unchanged.

---

#### B4: `compact_resume_and_fork_preserve_model_history_view` (compact_resume_fork.rs:152)

**What it tests:** After compact to resume to fork, model sees consistent history.

**Key break:** Expected user text order changes from `[original_user, summary, ...]` to
`[summary, ...preserved, ...]`.

**Fix:** Update expected user text arrays. The structural invariant
(compact_prefix is subset of resume_prefix is subset of fork_prefix) should still hold because
`replacement_history` is stored verbatim in the rollout.

---

#### B5: `compact_resume_after_second_compaction_preserves_history` (compact_resume_fork.rs:307)

**Similar to B4.** Update expected user text order.

---

## Implementation Checklist

### Step 1: Make the assembly change (5 minutes)

1. Remove `#[allow(unused_variables)]` from line 106
2. Replace lines 197-203 with the new code above
3. Do NOT add a synthetic model ack message
4. Do NOT touch anything else in the function

### Step 2: Run the tests and capture failures (2 minutes)

```bash
cd codex-rs
cargo test -p codex-core -- compact 2>&1 | tee /tmp/compact-test-output.txt
```

This will produce ~12 failures. Capture the output.

### Step 3: Inspect actual post-compact shapes (10 minutes)

Before trying to fix tests, run specific failing tests with `INSTA_UPDATE=new` to see
the actual new shapes:

```bash
cd codex-rs
INSTA_UPDATE=new cargo test -p codex-core -- multiple_auto_compact 2>&1 | head -100
```

For assertion-based tests, add temporary debug prints to see the actual shapes:

```rust
eprintln!("ACTUAL POST-COMPACT INPUT: {}", serde_json::to_string_pretty(&input).unwrap());
```

### Step 4: Accept snapshot changes (10 minutes)

```bash
cargo insta pending-snapshots -p codex-core
# Review each .snap.new file
cargo insta accept -p codex-core
```

### Step 5: Fix assertion-based tests (45 minutes)

Work through B1-B5 in order. For each test:
1. Run it in isolation to see the actual failure
2. Understand what the actual new shape is
3. Update assertions to match the new shape
4. Verify the test passes

### Step 6: Run full test suite

```bash
cd codex-rs
cargo test -p codex-core
```

### Step 7: Lint and format

```bash
cd codex-rs
just fmt
just fix -p codex-core
just argument-comment-lint
```

## Files to Touch

| File | Change |
|------|--------|
| `core/src/compact.rs` lines 106, 197-203 | Remove `#[allow(unused)]`, replace assembly |
| `core/tests/suite/compact.rs` | Update tests (snapshots + assertions) |
| `core/tests/suite/compact_resume_fork.rs` | Update tests (snapshots + assertions) |
| 7 `.snap` files in `core/tests/suite/snapshots/` | Re-recorded via `cargo insta accept` |

## Files NOT to Touch

| File | Reason |
|------|--------|
| `compact_tests.rs` | Tests remote path + split-point functions -- none call `run_compact_task_inner` |
| `codex_tests_guardian.rs` | Its compact test exercises the remote path only |
| `compact_remote.rs` | Remote compact has its own assembly |
| `rollout_reconstruction.rs` | Legacy path uses `build_compacted_history`; new compactions use `replacement_history: Some(...)` directly |
| `codex.rs` | Uses `collect_user_messages` for connector mentions, unrelated |
| `templates/compact/prompt.md` | Already the structured XML prompt |

## Key Gotchas from the Previous Failed Attempt

1. **DO NOT use `history_snapshot.raw_items()` for the split calculation.** It includes
   the compact prompt and LLM summary response. Use `pre_compact_items` (line 107).

2. **DO NOT add a synthetic assistant "ack" message.** It is unnecessary and changes
   the history shape in ways that break more tests.

3. **DO NOT change `build_compacted_history` or `collect_user_messages`.** They are still
   used by other code paths.

4. **DO review snapshot diffs before accepting.** The "Post-Compaction History Layout"
   sections should show the new shape: summary first, then preserved items.

5. **The `multiple_auto_compact` test is the hardest.** Its 400-line JSON blob needs
   updating. Consider restructuring to shape-agnostic assertions.

6. **When no user message exists in the 30% tail**, `find_compact_split_point` returns
   `items.len()` and preserved_items is empty. The post-compact history is just `[summary]`.
   Several tests hit this case because their history contains only tool calls + reasoning
   without user messages after the initial one. This is correct behavior -- it means the
   entire history was summarized.

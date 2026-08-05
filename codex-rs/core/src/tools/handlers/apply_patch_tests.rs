use super::*;
use codex_apply_patch::MaybeApplyPatchVerified;
use codex_exec_server::LOCAL_FS;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::protocol::FileChange;
use core_test_support::PathBufExt;
use core_test_support::PathExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;

use crate::session::tests::make_session_and_context;
use crate::tools::context::ToolInvocation;
use crate::tools::hook_names::HookToolName;
use crate::tools::registry::PostToolUsePayload;
use crate::tools::registry::PreToolUsePayload;
use crate::turn_diff_tracker::TurnDiffTracker;

fn sample_patch() -> &'static str {
    r#"*** Begin Patch
*** Add File: hello.txt
+hello
*** End Patch"#
}

async fn invocation_for_payload(payload: ToolPayload) -> ToolInvocation {
    let (session, turn) = make_session_and_context().await;
    ToolInvocation {
        session: session.into(),
        turn: turn.into(),
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        tracker: Arc::new(Mutex::new(TurnDiffTracker::new())),
        call_id: "call-apply-patch".to_string(),
        tool_name: codex_tools::ToolName::plain("apply_patch"),
        source: crate::tools::context::ToolCallSource::Direct,
        payload,
    }
}

#[tokio::test]
async fn pre_tool_use_payload_uses_freeform_patch_input() {
    let patch = sample_patch();
    let payload = ToolPayload::Custom {
        input: patch.to_string(),
    };
    let invocation = invocation_for_payload(payload).await;
    let handler = ApplyPatchHandler::default();

    assert_eq!(
        handler.pre_tool_use_payload(&invocation),
        Some(PreToolUsePayload {
            tool_name: HookToolName::apply_patch(),
            tool_input: json!({ "command": patch }),
        })
    );
}

#[tokio::test]
async fn post_tool_use_payload_uses_patch_input_and_tool_output() {
    let patch = sample_patch();
    let payload = ToolPayload::Custom {
        input: patch.to_string(),
    };
    let invocation = invocation_for_payload(payload).await;
    let output = ApplyPatchToolOutput::from_text("Success. Updated files.".to_string());
    let handler = ApplyPatchHandler::default();

    assert_eq!(
        handler.post_tool_use_payload(&invocation, &output),
        Some(PostToolUsePayload {
            tool_name: HookToolName::apply_patch(),
            tool_use_id: "call-apply-patch".to_string(),
            tool_input: json!({ "command": patch }),
            tool_response: json!("Success. Updated files."),
        })
    );
}

#[test]
fn diff_consumer_streams_apply_patch_changes() {
    let mut consumer = ApplyPatchArgumentDiffConsumer::default();
    assert!(
        consumer
            .push_delta("call-1".to_string(), "*** Begin Patch\n")
            .is_none()
    );

    let event = consumer
        .push_delta("call-1".to_string(), "*** Add File: hello.txt\n+hello")
        .expect("progress event");
    assert_eq!(
        (event.call_id, event.changes),
        (
            "call-1".to_string(),
            HashMap::from([(
                PathBuf::from("hello.txt"),
                FileChange::Add {
                    content: String::new(),
                },
            )]),
        )
    );

    assert!(
        consumer
            .push_delta("call-1".to_string(), "\n+world")
            .is_none()
    );
    assert!(
        consumer
            .push_delta("call-1".to_string(), "\n*** End Patch")
            .is_none()
    );

    let event = consumer
        .finish_update_on_complete()
        .expect("finish parser")
        .expect("progress event");
    assert_eq!(
        (event.call_id, event.changes),
        (
            "call-1".to_string(),
            HashMap::from([(
                PathBuf::from("hello.txt"),
                FileChange::Add {
                    content: "hello\nworld\n".to_string(),
                },
            )]),
        )
    );
}

#[test]
fn diff_consumer_streams_apply_patch_changes_with_environment_header() {
    let mut consumer = ApplyPatchArgumentDiffConsumer::default();
    assert!(
        consumer
            .push_delta(
                "call-1".to_string(),
                "*** Begin Patch\n*** Environment ID: remote\n",
            )
            .is_none()
    );

    let event = consumer
        .push_delta("call-1".to_string(), "*** Add File: hello.txt\n+hello")
        .expect("progress event");
    assert_eq!(
        event.changes,
        HashMap::from([(
            PathBuf::from("hello.txt"),
            FileChange::Add {
                content: String::new(),
            },
        )])
    );
}

#[test]
fn diff_consumer_sends_next_update_after_buffer_interval() {
    let mut consumer = ApplyPatchArgumentDiffConsumer::default();
    consumer.push_delta("call-1".to_string(), "*** Begin Patch\n");
    let first = consumer
        .push_delta("call-1".to_string(), "*** Add File: hello.txt\n+hello")
        .expect("first progress event");
    assert_eq!(
        first.changes,
        HashMap::from([(
            PathBuf::from("hello.txt"),
            FileChange::Add {
                content: String::new(),
            },
        )])
    );

    consumer.last_sent_at =
        Some(std::time::Instant::now() - APPLY_PATCH_ARGUMENT_DIFF_BUFFER_INTERVAL);
    let second = consumer
        .push_delta("call-1".to_string(), "\n+world")
        .expect("second progress event");
    assert_eq!(
        second.changes,
        HashMap::from([(
            PathBuf::from("hello.txt"),
            FileChange::Add {
                content: "hello\n".to_string(),
            },
        )])
    );
}

#[test]
fn reconcile_environment_id_requires_selection_when_enabled() {
    assert_eq!(
        require_environment_id(Some("remote"), /*allow_environment_id*/ false),
        Err(FunctionCallError::RespondToModel(
            "apply_patch environment selection is unavailable for this turn".to_string(),
        ))
    );
    assert_eq!(
        require_environment_id(
            /*parsed_environment_id*/ None, /*allow_environment_id*/ true
        ),
        Ok(None)
    );
}

#[tokio::test]
async fn approval_keys_include_move_destination() {
    let tmp = TempDir::new().expect("tmp");
    let cwd_path = tmp.path();
    let cwd = cwd_path.abs();
    std::fs::create_dir_all(cwd_path.join("old")).expect("create old dir");
    std::fs::create_dir_all(cwd_path.join("renamed/dir")).expect("create dest dir");
    std::fs::write(cwd_path.join("old/name.txt"), "old content\n").expect("write old file");
    let patch = r#"*** Begin Patch
*** Update File: old/name.txt
*** Move to: renamed/dir/name.txt
@@
-old content
+new content
*** End Patch"#;
    let argv = vec!["apply_patch".to_string(), patch.to_string()];
    let action = match codex_apply_patch::maybe_parse_apply_patch_verified(
        &argv,
        &cwd,
        LOCAL_FS.as_ref(),
        /*sandbox*/ None,
    )
    .await
    {
        MaybeApplyPatchVerified::Body(action) => action,
        other => panic!("expected patch body, got: {other:?}"),
    };

    let keys = file_paths_for_action(&action);
    assert_eq!(keys.len(), 2);
}

#[test]
fn write_permissions_for_paths_skip_dirs_already_writable_under_workspace_root() {
    let tmp = TempDir::new().expect("tmp");
    let cwd_path = tmp.path();
    let cwd = cwd_path.abs();
    let nested = cwd_path.join("nested");
    std::fs::create_dir_all(&nested).expect("create nested dir");
    let file_path = AbsolutePathBuf::try_from(nested.join("file.txt"))
        .expect("nested file path should be absolute");
    let sandbox_policy = FileSystemSandboxPolicy::workspace_write(
        &[],
        /*exclude_tmpdir_env_var*/ true,
        /*exclude_slash_tmp*/ false,
    );

    let permissions = write_permissions_for_paths(&[file_path], &sandbox_policy, &cwd);

    assert_eq!(permissions, None);
}

#[test]
fn write_permissions_for_paths_keep_dirs_outside_workspace_root() {
    let tmp = TempDir::new().expect("tmp");
    let cwd = tmp.path().join("workspace");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&cwd).expect("create cwd");
    std::fs::create_dir_all(&outside).expect("create outside dir");
    let file_path = AbsolutePathBuf::try_from(outside.join("file.txt"))
        .expect("outside file path should be absolute");
    let cwd_abs = cwd.abs();
    let sandbox_policy = FileSystemSandboxPolicy::workspace_write(
        &[],
        /*exclude_tmpdir_env_var*/ true,
        /*exclude_slash_tmp*/ true,
    );

    let permissions = write_permissions_for_paths(&[file_path], &sandbox_policy, &cwd_abs);
    let expected_outside =
        dunce::simplified(&outside.canonicalize().expect("canonicalize outside dir")).abs();

    assert_eq!(
        permissions
            .and_then(|profile| profile.file_system)
            .and_then(|fs| fs.legacy_read_write_roots())
            .and_then(|(_read, write)| write),
        Some(vec![expected_outside])
    );
}

// ────────────────────────────────────────────────────────────────────────────
// REGRESSION GUARDS — wire-translation symmetry for apply_patch
//
// These tests fail loudly the moment apply_patch stops accepting a translated
// `ToolPayload::Function { arguments: {"input": "..."} }` payload from a
// non-`/responses` wire (Anthropic /messages, Copilot /chat/completions,
// Gemini generateContent). Without them, every apply_patch call on those
// wires hits `Fatal error: tool apply_patch invoked with incompatible payload`
// in `registry.rs::dispatch_tool_call_inner`, the tool task is dropped on
// the floor, and the user sees only "aborted" with no diagnostic — the
// exact 2026-06-07 silent regression that motivated this guard.
//
// Why both shapes must be accepted: the apply_patch tool spec is declared
// freeform (lark grammar) for /responses, then translated by
// `provider-anthropic/src/wire.rs::tools_to_anthropic_format` into a regular
// function tool with a single `input: string` parameter for every wire that
// lacks native freeform support. The SSE-side parser emits `FunctionCall`
// for translated tool_use blocks, the router maps that to
// `ToolPayload::Function`, and the handler must accept it.
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn apply_patch_accepts_translated_function_payload() {
    let patch = sample_patch();
    // Shape emitted by Anthropic/Copilot/Gemini wires after Freeform→Function
    // translation: a regular function call whose JSON arguments wrap the
    // patch text in `{"input": "..."}` per the translated tool schema.
    let translated = ToolPayload::Function {
        arguments: json!({ "input": patch }).to_string(),
    };
    let handler = ApplyPatchHandler::default();

    assert!(
        handler.matches_kind(&translated),
        "ApplyPatchHandler must accept translated Function payloads from \
         non-/responses wires (Anthropic /messages, Copilot, Gemini). \
         Without this, every apply_patch call on those wires aborts with \
         `tool apply_patch invoked with incompatible payload`."
    );

    // The hook-input extractor must work on both shapes too — otherwise
    // pre/post_tool_use hooks silently drop translated calls even after
    // matches_kind passes.
    assert_eq!(
        apply_patch_input_from_payload(&translated),
        Some(patch.to_string()),
        "apply_patch_input_from_payload must unwrap Function payload's \
         `{{\"input\": \"...\"}}` arguments. If this fails, registry-level \
         dispatch still succeeds but PreToolUse/PostToolUse hooks see no \
         input and skip the call."
    );
}

#[tokio::test]
async fn apply_patch_still_accepts_freeform_custom_payload() {
    // Pair with the regression test above — confirms accepting Function
    // didn't break the /responses-native Custom path.
    let patch = sample_patch();
    let custom = ToolPayload::Custom {
        input: patch.to_string(),
    };
    let handler = ApplyPatchHandler::default();

    assert!(handler.matches_kind(&custom));
    assert_eq!(
        apply_patch_input_from_payload(&custom),
        Some(patch.to_string())
    );
}

#[tokio::test]
async fn apply_patch_rejects_unrelated_function_payload() {
    // Belt-and-suspenders: a Function payload that does NOT carry an `input`
    // string field must still be rejected, or matches_kind would over-match
    // and the handler would try to parse arbitrary JSON as a patch grammar.
    let bad = ToolPayload::Function {
        arguments: json!({ "command": "ls" }).to_string(),
    };
    let handler = ApplyPatchHandler::default();

    assert!(
        !handler.matches_kind(&bad),
        "ApplyPatchHandler must reject Function payloads lacking the \
         translated `input: string` field — otherwise the handler would \
         accept arbitrary tool calls and try to parse their JSON as a patch."
    );
    assert_eq!(apply_patch_input_from_payload(&bad), None);
}

#[tokio::test]
async fn apply_patch_rejects_malformed_function_arguments() {
    // Non-JSON arguments must be rejected without panic.
    let bad = ToolPayload::Function {
        arguments: "not json at all".to_string(),
    };
    let handler = ApplyPatchHandler::default();
    assert!(!handler.matches_kind(&bad));
    assert_eq!(apply_patch_input_from_payload(&bad), None);
}

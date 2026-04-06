use super::*;
use std::collections::HashSet;

/// Helper: build a masker backed by `dir` with the given threshold and no
/// exempt tools (unless specified).
fn masker_in(dir: &Path, threshold: usize) -> ToolOutputMasker {
    ToolOutputMasker::new(dir.to_path_buf(), threshold, HashSet::new())
}

fn masker_in_with_exemptions(
    dir: &Path,
    threshold: usize,
    exempt: &[&str],
) -> ToolOutputMasker {
    let exempt_set: HashSet<String> = exempt.iter().map(|s| s.to_string()).collect();
    ToolOutputMasker::new(dir.to_path_buf(), threshold, exempt_set)
}

// ── Test 1: Short output (< threshold) → unmasked ──────────────────────

#[test]
fn short_output_is_unmasked() {
    let tmp = tempfile::tempdir().unwrap();
    let masker = masker_in(tmp.path(), 100);
    let output = "hello world";

    let result = masker.maybe_mask("some_tool", output).unwrap();

    assert_eq!(result, MaskResult::Unmasked("hello world".to_string()));
}

// ── Test 2: Long output (> threshold) → masked with head+tail ──────────

#[test]
fn long_output_is_masked_with_head_and_tail() {
    let tmp = tempfile::tempdir().unwrap();
    let masker = masker_in(tmp.path(), 100);

    // Build a string of 500 chars: "aaaa...bbbb..."
    let head_part = "a".repeat(200);
    let middle = "m".repeat(100);
    let tail_part = "z".repeat(200);
    let output = format!("{head_part}{middle}{tail_part}");
    assert_eq!(output.len(), 500);

    let result = masker.maybe_mask("some_tool", &output).unwrap();

    match &result {
        MaskResult::Masked {
            replacement,
            original_path,
        } => {
            // The replacement should contain the XML wrapper.
            assert!(
                replacement.starts_with("<tool_output_masked ref=\""),
                "replacement should start with XML tag, got: {replacement}"
            );
            assert!(
                replacement.ends_with("</tool_output_masked>"),
                "replacement should end with closing XML tag"
            );

            // It should contain the head (first 200 chars).
            assert!(
                replacement.contains(&head_part),
                "replacement should contain head preview"
            );

            // It should contain the tail (last 200 chars).
            assert!(
                replacement.contains(&tail_part),
                "replacement should contain tail preview"
            );

            // It should mention how many chars were masked.
            assert!(
                replacement.contains("100 chars masked"),
                "replacement should state masked char count"
            );

            // The original_path should be inside the temp dir.
            assert!(
                original_path.starts_with(tmp.path()),
                "original_path should be inside mask_dir"
            );
        }
        MaskResult::Unmasked(_) => {
            panic!("expected Masked, got Unmasked");
        }
    }
}

// ── Test 3: Exempt tool → never masked regardless of size ──────────────

#[test]
fn exempt_tool_is_never_masked() {
    let tmp = tempfile::tempdir().unwrap();
    let masker =
        masker_in_with_exemptions(tmp.path(), 10, &["ask_user_question", "memory"]);

    let huge_output = "x".repeat(100_000);

    let result = masker
        .maybe_mask("ask_user_question", &huge_output)
        .unwrap();
    assert_eq!(result, MaskResult::Unmasked(huge_output.clone()));

    let result2 = masker.maybe_mask("memory", &huge_output).unwrap();
    assert_eq!(result2, MaskResult::Unmasked(huge_output));
}

// ── Test 4: Mask file written to disk with full output ─────────────────

#[test]
fn mask_file_written_to_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let masker = masker_in(tmp.path(), 100);

    let output = "d".repeat(500);
    let result = masker.maybe_mask("some_tool", &output).unwrap();

    match result {
        MaskResult::Masked { original_path, .. } => {
            assert!(original_path.exists(), "mask file should exist on disk");
            let contents = std::fs::read_to_string(&original_path).unwrap();
            assert_eq!(
                contents, output,
                "mask file should contain the full original output"
            );
        }
        MaskResult::Unmasked(_) => {
            panic!("expected Masked, got Unmasked");
        }
    }
}

// ── Additional coverage ────────────────────────────────────────────────

#[test]
fn sha256_hex_is_deterministic() {
    let a = sha256_hex("hello");
    let b = sha256_hex("hello");
    assert_eq!(a, b);
    // Known SHA-256 of "hello".
    assert_eq!(
        a,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
}

#[test]
fn with_defaults_sets_expected_exemptions() {
    let masker = ToolOutputMasker::with_defaults();
    assert!(masker.is_exempt("ask_user_question"));
    assert!(masker.is_exempt("memory"));
    assert!(!masker.is_exempt("bash"));
}

#[test]
fn add_exempt_tool_works() {
    let mut masker = ToolOutputMasker::with_defaults();
    assert!(!masker.is_exempt("my_skill"));
    masker.add_exempt_tool("my_skill");
    assert!(masker.is_exempt("my_skill"));
}

#[test]
fn exactly_at_threshold_is_masked() {
    // The condition is `< threshold`, so exactly-at-threshold is NOT
    // shorter and therefore gets masked.
    let tmp = tempfile::tempdir().unwrap();
    let masker = masker_in(tmp.path(), 100);
    let output = "x".repeat(100);
    let result = masker.maybe_mask("tool", &output).unwrap();
    assert!(matches!(result, MaskResult::Masked { .. }));
}

#[test]
fn one_below_threshold_is_unmasked() {
    let tmp = tempfile::tempdir().unwrap();
    let masker = masker_in(tmp.path(), 100);
    let output = "x".repeat(99); // strictly below threshold
    let result = masker.maybe_mask("tool", &output).unwrap();
    assert!(matches!(result, MaskResult::Unmasked(_)));
}

#[test]
fn one_above_threshold_is_masked() {
    let tmp = tempfile::tempdir().unwrap();
    let masker = masker_in(tmp.path(), 100);
    let output = "x".repeat(101); // one above threshold
    let result = masker.maybe_mask("tool", &output).unwrap();
    assert!(matches!(result, MaskResult::Masked { .. }));
}

#[test]
fn mask_dir_is_created_on_demand() {
    let tmp = tempfile::tempdir().unwrap();
    let nested = tmp.path().join("a").join("b").join("c");
    assert!(!nested.exists());

    let masker = masker_in(&nested, 10);
    let output = "x".repeat(100);
    masker.maybe_mask("tool", &output).unwrap();

    assert!(nested.exists(), "nested mask_dir should be auto-created");
}

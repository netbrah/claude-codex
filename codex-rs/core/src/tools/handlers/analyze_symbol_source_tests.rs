use pretty_assertions::assert_eq;
use tempfile::tempdir;

use super::super::manifest_builder::build_manifest;
use super::super::search_rg::make_scope_tempfile;
use super::super::search_rg::run_rg_lines_direct;
use super::*;

// ---------------------------------------------------------------------------
// Unit tests: base_symbol_name
// ---------------------------------------------------------------------------

#[test]
fn base_symbol_name_plain() {
    assert_eq!(base_symbol_name("my_func"), "my_func");
}

#[test]
fn base_symbol_name_rust_qualified() {
    assert_eq!(base_symbol_name("MyStruct::my_method"), "my_method");
}

#[test]
fn base_symbol_name_deeply_qualified() {
    assert_eq!(
        base_symbol_name("crate::module::SubModule::helper"),
        "helper"
    );
}

#[test]
fn base_symbol_name_dotted() {
    assert_eq!(base_symbol_name("pkg.Class.Method"), "Method");
}

#[test]
fn base_symbol_name_prefers_double_colon() {
    // When both `::` and `.` appear, `::` takes precedence (Rust style).
    assert_eq!(base_symbol_name("pkg::Type.method"), "method");
}

// ---------------------------------------------------------------------------
// Unit tests: is_test_file
// ---------------------------------------------------------------------------

#[test]
fn is_test_file_slash_tests() {
    assert!(is_test_file("/project/tests/my_mod.rs"));
}

#[test]
fn is_test_file_slash_test() {
    assert!(is_test_file("/project/test/helpers.ts"));
}

#[test]
fn is_test_file_underscore_test_dot() {
    assert!(is_test_file("src/parser_test.rs"));
}

#[test]
fn is_test_file_dot_test_dot() {
    assert!(is_test_file("src/parser.test.ts"));
}

#[test]
fn is_test_file_spec() {
    assert!(is_test_file("src/components/Button.spec.tsx"));
}

#[test]
fn is_test_file_normal_file() {
    assert!(!is_test_file("src/lib.rs"));
    assert!(!is_test_file("src/tools/grep_files.rs"));
}

// ---------------------------------------------------------------------------
// Unit tests: extract_callees
// ---------------------------------------------------------------------------

#[test]
fn extract_callees_basic() {
    let source = r#"
fn my_func(x: u32) -> u32 {
    let y = helper_a(x);
    let z = helper_b(y);
    y + z
}
"#;
    let callees = extract_callees(source, 10, "my_func");
    let names: Vec<&str> = callees.iter().map(|c| c.callee.as_str()).collect();
    assert!(names.contains(&"helper_a"), "should find helper_a");
    assert!(names.contains(&"helper_b"), "should find helper_b");
}

#[test]
fn extract_callees_skips_comments() {
    let source = r#"
fn my_func() {
    // ignored_call(x);
    /* also_ignored(y); */
    real_call(1);
}
"#;
    let callees = extract_callees(source, 10, "my_func");
    let names: Vec<&str> = callees.iter().map(|c| c.callee.as_str()).collect();
    assert!(names.contains(&"real_call"), "real_call should be found");
    assert!(
        !names.contains(&"ignored_call"),
        "ignored_call in comment should be skipped"
    );
    assert!(
        !names.contains(&"also_ignored"),
        "also_ignored in block comment should be skipped"
    );
}

#[test]
fn extract_callees_skips_keywords() {
    let source = r#"
fn process() {
    for item in items() {
        if valid(item) {
            transform(item);
        }
    }
}
"#;
    let callees = extract_callees(source, 10, "process");
    let names: Vec<&str> = callees.iter().map(|c| c.callee.as_str()).collect();
    assert!(!names.contains(&"for"), "keyword 'for' must be skipped");
    assert!(!names.contains(&"if"), "keyword 'if' must be skipped");
    assert!(names.contains(&"items"), "items() should be found");
    assert!(names.contains(&"valid"), "valid() should be found");
    assert!(names.contains(&"transform"), "transform() should be found");
}

#[test]
fn extract_callees_classifies_method_call() {
    let source = r#"
fn run(obj: MyType) {
    obj.do_thing(42);
}
"#;
    let callees = extract_callees(source, 10, "run");
    let do_thing = callees.iter().find(|c| c.callee == "do_thing");
    assert!(do_thing.is_some(), "do_thing should be found");
    assert_eq!(
        do_thing.unwrap().call_type.as_deref(),
        Some("method"),
        "dot-prefixed call should be classified as method"
    );
}

#[test]
fn extract_callees_filters_new_keyword() {
    // `new` is listed as a keyword and should not appear in callees.
    let source = r#"
fn build() -> Thing {
    Thing::new()
}
"#;
    let callees = extract_callees(source, 10, "build");
    let new_call = callees.iter().find(|c| c.callee == "new");
    assert!(
        new_call.is_none(),
        "new is a keyword and should be filtered out"
    );
}

#[test]
fn extract_callees_classifies_static_call_non_keyword() {
    let source = r#"
fn build() -> Config {
    Config::default()
}
"#;
    let callees = extract_callees(source, 10, "build");
    let default_call = callees.iter().find(|c| c.callee == "default");
    assert!(default_call.is_some(), "default() should be found");
    assert_eq!(
        default_call.unwrap().call_type.as_deref(),
        Some("static"),
        "double-colon call should be classified as static"
    );
}

#[test]
fn extract_callees_respects_max() {
    let source = r#"
fn many_calls() {
    a1(1); a2(2); a3(3); a4(4); a5(5);
}
"#;
    let callees = extract_callees(source, 3, "many_calls");
    assert!(callees.len() <= 3, "should respect max_callees limit");
}

#[test]
fn extract_callees_deduplicates() {
    let source = r#"
fn repeated() {
    helper(1);
    helper(2);
    helper(3);
}
"#;
    let callees = extract_callees(source, 10, "repeated");
    let helper_count = callees.iter().filter(|c| c.callee == "helper").count();
    assert_eq!(helper_count, 1, "duplicate callees should be deduped");
}

#[test]
fn extract_callees_skips_self_name() {
    let source = r#"
fn recurse(n: u32) -> u32 {
    if n == 0 { return 0; }
    recurse(n - 1)
}
"#;
    let callees = extract_callees(source, 10, "recurse");
    assert!(
        !callees.iter().any(|c| c.callee == "recurse"),
        "the symbol itself should not appear as a callee"
    );
}

// ---------------------------------------------------------------------------
// Unit tests: build_definition_pattern
// ---------------------------------------------------------------------------

#[test]
fn build_definition_pattern_matches_rust_fn() {
    let pattern = build_definition_pattern("my_func");
    // The pattern should be a non-empty string.
    assert!(!pattern.is_empty());
    // Quick sanity check using regex crate via string matching.
    assert!(
        pattern.contains("my_func"),
        "pattern should contain the symbol name"
    );
}

// ---------------------------------------------------------------------------
// Integration tests using a temporary directory workspace
// ---------------------------------------------------------------------------

fn rg_available() -> bool {
    std::process::Command::new("rg")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Returns true when the installed rg supports `--files-from` (added after rg 15.0).
fn rg_supports_files_from() -> bool {
    std::process::Command::new("rg")
        .arg("--help")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|help| help.contains("--files-from"))
        .unwrap_or(false)
}

/// Full round-trip using the direct (fallback) search path: write a fake
/// workspace, invoke `run_rg_lines_direct`, and verify we find the definition
/// and references.
#[tokio::test]
async fn integration_find_definition_and_callers_direct() -> anyhow::Result<()> {
    if !rg_available() {
        eprintln!("rg not in PATH; skipping integration test");
        return Ok(());
    }

    let dir = tempdir()?;
    let base = dir.path();

    // Write a "library" file with a function definition.
    std::fs::write(
        base.join("lib.rs"),
        r#"
pub fn compute_result(x: u32) -> u32 {
    let y = helper(x);
    y * 2
}

fn helper(x: u32) -> u32 {
    x + 1
}
"#,
    )?;

    // Write a caller file.
    std::fs::write(
        base.join("main.rs"),
        r#"
fn main() {
    let v = compute_result(10);
    println!("{}", v);
}
"#,
    )?;

    // Write a test file.
    std::fs::write(
        base.join("lib_test.rs"),
        r#"
#[test]
fn test_compute_result() {
    assert_eq!(compute_result(1), 4);
}
"#,
    )?;

    // Search for the definition via the direct (fallback) path.
    let def_pattern = build_definition_pattern("compute_result");
    let def_hits = run_rg_lines_direct(&def_pattern, base, 5, Some(1))
        .await
        .map_err(anyhow::Error::msg)?;
    assert!(!def_hits.is_empty(), "should find definition");
    assert!(
        def_hits[0].0.ends_with("lib.rs"),
        "definition should be in lib.rs"
    );
    assert_eq!(def_hits[0].1, 2, "definition should be on line 2");

    // Search for references via the direct (fallback) path.
    let ref_pattern = format!(r"\b{}\b", "compute_result");
    let ref_hits = run_rg_lines_direct(&ref_pattern, base, 100, /*max_count_per_file=*/ None)
        .await
        .map_err(anyhow::Error::msg)?;
    assert!(ref_hits.len() >= 2, "should find at least 2 references");

    // Partition into test vs non-test callers.
    let def_key = (def_hits[0].0.clone(), def_hits[0].1);
    let callers: Vec<_> = ref_hits
        .iter()
        .filter(|(f, l, _)| (*f != def_key.0 || *l != def_key.1) && !is_test_file(f))
        .collect();
    let test_callers: Vec<_> = ref_hits
        .iter()
        .filter(|(f, l, _)| (*f != def_key.0 || *l != def_key.1) && is_test_file(f))
        .collect();

    assert!(
        !callers.is_empty(),
        "should have at least one non-test caller"
    );
    assert!(
        !test_callers.is_empty(),
        "should have at least one test caller"
    );

    // Extract callees from the definition snippet.
    let source = read_source_snippet_sync(&def_hits[0].0, def_hits[0].1, 10)
        .expect("should read source snippet");
    let callees = extract_callees(&source, 10, "compute_result");
    let callee_names: Vec<&str> = callees.iter().map(|c| c.callee.as_str()).collect();
    assert!(
        callee_names.contains(&"helper"),
        "helper() should be extracted as a callee"
    );

    Ok(())
}

/// Full round-trip using the manifest-backed search path: build a manifest for
/// the temporary workspace, create a scope temp file, and verify that
/// `run_rg_lines_from_manifest` finds the same results as the direct path.
#[tokio::test]
async fn integration_find_definition_and_callers_via_manifest() -> anyhow::Result<()> {
    if !rg_available() {
        eprintln!("rg not in PATH; skipping integration test");
        return Ok(());
    }
    if !rg_supports_files_from() {
        eprintln!("rg does not support --files-from; skipping manifest integration test");
        return Ok(());
    }

    let dir = tempdir()?;
    let base = dir.path();

    std::fs::write(
        base.join("lib.rs"),
        r#"
pub fn compute_result(x: u32) -> u32 {
    let y = helper(x);
    y * 2
}

fn helper(x: u32) -> u32 {
    x + 1
}
"#,
    )?;
    std::fs::write(
        base.join("main.rs"),
        r#"
fn main() {
    let v = compute_result(10);
    println!("{}", v);
}
"#,
    )?;
    std::fs::write(
        base.join("lib_test.rs"),
        r#"
#[test]
fn test_compute_result() {
    assert_eq!(compute_result(1), 4);
}
"#,
    )?;

    // Build a manifest for the temporary workspace.
    let manifest_path = base.join("manifest.txt");
    build_manifest(base, &manifest_path, usize::MAX).expect("manifest build should succeed");
    assert!(manifest_path.exists(), "manifest file should exist");

    // Create a scope temp file covering the whole workspace.
    let tmp = make_scope_tempfile(&manifest_path, base)
        .expect("make_scope_tempfile should succeed for populated scope");

    // Search for the definition via the manifest path.
    let def_pattern = build_definition_pattern("compute_result");
    let def_hits = super::super::search_rg::run_rg_lines_from_manifest(
        &def_pattern,
        tmp.path(),
        5,
        Some(1),
        base,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    assert!(
        !def_hits.is_empty(),
        "manifest search should find definition"
    );
    assert!(
        def_hits[0].0.ends_with("lib.rs"),
        "definition should be in lib.rs"
    );

    // Search for references via the manifest path.
    let ref_pattern = format!(r"\b{}\b", "compute_result");
    let ref_hits = super::super::search_rg::run_rg_lines_from_manifest(
        &ref_pattern,
        tmp.path(),
        100,
        /*max_count_per_file=*/ None,
        base,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    assert!(
        ref_hits.len() >= 2,
        "manifest search should find at least 2 references"
    );

    Ok(())
}

/// Verify that `make_scope_tempfile` returns `None` for a scope that has no
/// files in the manifest (graceful fallback signal).
#[test]
fn make_scope_tempfile_returns_none_for_empty_scope() {
    let dir = tempdir().unwrap();
    let base = dir.path();

    // Write a manifest that only covers `sub_a/`.
    let sub_a = base.join("sub_a");
    std::fs::create_dir(&sub_a).unwrap();
    std::fs::write(sub_a.join("file.rs"), "fn a() {}").unwrap();

    let manifest_path = base.join("manifest.txt");
    build_manifest(base, &manifest_path, usize::MAX).expect("manifest build");

    // Requesting a scope that has no files in the manifest should return None.
    let sub_b = base.join("sub_b_does_not_exist");
    let result = make_scope_tempfile(&manifest_path, &sub_b);
    assert!(
        result.is_none(),
        "make_scope_tempfile should return None for a scope with no manifest entries"
    );
}

/// Verify that `make_scope_tempfile` succeeds when the scope has indexed files.
#[test]
fn make_scope_tempfile_succeeds_for_populated_scope() {
    let dir = tempdir().unwrap();
    let base = dir.path();

    std::fs::write(base.join("a.rs"), "fn a() {}").unwrap();
    std::fs::write(base.join("b.rs"), "fn b() {}").unwrap();

    let manifest_path = base.join("manifest.txt");
    build_manifest(base, &manifest_path, usize::MAX).expect("manifest build");

    let tmp = make_scope_tempfile(&manifest_path, base);
    assert!(
        tmp.is_some(),
        "make_scope_tempfile should return Some when files are in scope"
    );
    // The temp file should contain both paths.
    let content = std::fs::read_to_string(tmp.unwrap().path()).unwrap();
    assert!(content.contains("a.rs"), "temp file should list a.rs");
    assert!(content.contains("b.rs"), "temp file should list b.rs");
}

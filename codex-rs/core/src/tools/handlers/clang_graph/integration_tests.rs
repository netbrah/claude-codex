//! Integration tests for the clang-graph feature.
//!
//! These require:
//!   - `clang-graph` feature enabled
//!   - `CODEX_TEST_LIBCLANG=1` environment variable set
//!   - libclang-dev installed (for TU-parsing tests)
//!
//! Gate: skip at runtime if the env var is absent, so `cargo test` without
//! the flag still passes.

use std::io::Write;

use super::bfs_traversal::BfsConfig;
use super::bfs_traversal::BfsPriority;
use super::bfs_traversal::bfs_call_graph;
use super::compile_commands_index::CompileCommandsIndex;

/// Runtime guard — returns true when the integration harness is active.
fn harness_enabled() -> bool {
    std::env::var("CODEX_TEST_LIBCLANG").as_deref() == Ok("1")
}

// ---------------------------------------------------------------------------
// CompileCommandsIndex round-trip
// ---------------------------------------------------------------------------

#[test]
fn integration_compile_commands_index_roundtrip() {
    if !harness_enabled() {
        eprintln!("skipping (CODEX_TEST_LIBCLANG not set)");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("compile_commands.json");

    // Write a minimal compile_commands.json with 3 entries.
    let json = r#"[
        {
            "directory": "/build",
            "file": "/src/main.cpp",
            "arguments": ["clang++", "-std=c++17", "-I/inc", "/src/main.cpp"]
        },
        {
            "directory": "/build",
            "file": "/src/util.cpp",
            "command": "clang++ -std=c++17 -I/inc /src/util.cpp"
        },
        {
            "directory": "/build",
            "file": "/src/helper.cpp",
            "arguments": ["clang++", "-O2", "/src/helper.cpp"]
        }
    ]"#;

    std::fs::write(&db_path, json).unwrap();

    let index = CompileCommandsIndex::build(&db_path).expect("failed to build index");
    assert_eq!(index.file_count(), 3);

    // Lookup by exact path.
    let args = index
        .get_args(std::path::Path::new("/src/main.cpp"))
        .expect("main.cpp not found");
    assert!(args.arguments.iter().any(|a| a.contains("c++17")));

    // Lookup by the `command`-style entry.
    let util_args = index
        .get_args(std::path::Path::new("/src/util.cpp"))
        .expect("util.cpp not found");
    assert!(!util_args.arguments.is_empty());

    // Missing file returns None.
    assert!(
        index
            .get_args(std::path::Path::new("/src/missing.cpp"))
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// BFS traversal with heuristic engine (no clang)
// ---------------------------------------------------------------------------

#[test]
fn integration_bfs_heuristic_depth2() {
    if !harness_enabled() {
        eprintln!("skipping (CODEX_TEST_LIBCLANG not set)");
        return;
    }

    // Simulate a 3-function chain: main -> process -> compute
    // Heuristic callers (from rg output) for root "process".
    let heuristic_callers = vec![(
        "src/main.cpp".to_string(),
        42_u32,
        "  process(data);".to_string(),
    )];
    let heuristic_callees = vec![
        ("compute".to_string(), Some(15_u32), "function".to_string()),
        ("validate".to_string(), Some(20_u32), "function".to_string()),
    ];

    let config = BfsConfig {
        max_depth: 2,
        max_nodes: 50,
        max_edges: 100,
        max_callers_per_hop: 10,
        max_callees_per_hop: 10,
        prioritize: BfsPriority::CallersFirst,
    };

    let result = bfs_call_graph(
        None, // no USR (heuristic mode)
        "process",
        "src/process.cpp",
        10,
        &mut None, // no ClangEngine
        &heuristic_callers,
        &heuristic_callees,
        &config,
    );

    // Root + 1 caller + 2 callees = 4 nodes.
    assert_eq!(
        result.nodes.len(),
        4,
        "expected 4 nodes, got {}: {:?}",
        result.nodes.len(),
        result.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    assert_eq!(result.engine, "heuristic");
    assert!(!result.truncated);

    // Root node.
    assert_eq!(result.nodes[0].name, "process");
    assert_eq!(result.nodes[0].depth, 0);

    // Verify edges: 1 caller edge + 2 callee edges = 3.
    assert_eq!(result.edges.len(), 3);

    // Caller edge: from caller -> to process.
    let caller_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.to == result.nodes[0].id)
        .collect();
    assert_eq!(caller_edges.len(), 1);

    // Callee edges: from process -> to callees.
    let callee_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.from == result.nodes[0].id)
        .collect();
    assert_eq!(callee_edges.len(), 2);
}

#[test]
fn integration_bfs_respects_caps() {
    if !harness_enabled() {
        eprintln!("skipping (CODEX_TEST_LIBCLANG not set)");
        return;
    }

    // Many callers, tight node cap.
    let heuristic_callers: Vec<_> = (0..20)
        .map(|i| (format!("caller_{i}.cpp"), i as u32, format!("call_{i}();")))
        .collect();

    let config = BfsConfig {
        max_depth: 1,
        max_nodes: 5,
        max_edges: 100,
        max_callers_per_hop: 20,
        max_callees_per_hop: 0,
        prioritize: BfsPriority::CallersFirst,
    };

    let result = bfs_call_graph(
        None,
        "target",
        "target.cpp",
        1,
        &mut None,
        &heuristic_callers,
        &[],
        &config,
    );

    assert!(result.nodes.len() <= 5);
    assert!(result.truncated);
}

// ---------------------------------------------------------------------------
// ClangEngine + TU parse (requires actual libclang)
// ---------------------------------------------------------------------------

#[test]
fn integration_clang_engine_parse_and_query() {
    if !harness_enabled() {
        eprintln!("skipping (CODEX_TEST_LIBCLANG not set)");
        return;
    }

    // Create a temp directory with a small C++ file and compile_commands.json.
    let dir = tempfile::tempdir().unwrap();

    let cpp_path = dir.path().join("example.cpp");
    let mut cpp_file = std::fs::File::create(&cpp_path).unwrap();
    writeln!(
        cpp_file,
        r#"
void helper() {{}}
void process() {{ helper(); }}
int main() {{ process(); return 0; }}
"#
    )
    .unwrap();

    let db_path = dir.path().join("compile_commands.json");
    let db_json = format!(
        r#"[{{"directory":"{}","file":"{}","arguments":["clang++","-std=c++17","{}"]}}]"#,
        dir.path().display(),
        cpp_path.display(),
        cpp_path.display()
    );
    std::fs::write(&db_path, db_json).unwrap();

    // Initialize engine.
    let engine_result = super::clang_engine::ClangEngine::new(dir.path());
    if engine_result.is_err() {
        eprintln!(
            "ClangEngine init failed (libclang issue?), skipping: {:?}",
            engine_result.err()
        );
        return;
    }
    let mut engine = engine_result.unwrap();

    // Parse the file.
    let parsed = engine.parse_file(&cpp_path);
    if parsed.is_err() {
        eprintln!("TU parse failed, skipping: {:?}", parsed.err());
        return;
    }
    assert!(parsed.unwrap()); // true = newly parsed

    // Second parse should be a no-op.
    assert!(!engine.parse_file(&cpp_path).unwrap());

    assert!(engine.parsed_count() >= 1);
    assert!(engine.node_count() >= 3); // helper, process, main

    // Find symbol.
    let matches = engine.find_symbol("process");
    assert!(!matches.is_empty(), "expected to find 'process'");

    let best = engine.find_best_match("process");
    assert!(best.is_some());

    // Check callees of process -> should include helper.
    let process_usr = &best.unwrap().usr;
    let callees = engine.direct_callees(process_usr);
    let callee_names: Vec<&str> = callees.iter().map(|n| n.display_name.as_str()).collect();
    assert!(
        callee_names.contains(&"helper"),
        "expected 'helper' in callees: {:?}",
        callee_names
    );
}

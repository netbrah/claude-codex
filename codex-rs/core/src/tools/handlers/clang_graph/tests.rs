//! Unit tests for the call graph module.
//!
//! These tests exercise the graph data structure and traversal without
//! requiring libclang (no actual C++ parsing). The edge extractor is
//! tested via the graph by manually constructing CallEdge structs.

use super::edge_extractor::CallEdge;
use super::graph::CachedEdges;
use super::graph::CallGraph;
use super::graph::FunctionNode;
use super::graph::SerializableEdge;
use super::graph::TraversalOrder;
use super::graph::load_edge_cache;
use super::graph::save_edge_cache;

fn make_edge(caller: &str, callee: &str) -> CallEdge {
    CallEdge {
        caller_usr: format!("{caller}@test.cpp:0"),
        caller_name: caller.to_string(),
        caller_file: "test.cpp".to_string(),
        callee_usr: format!("{callee}@test.cpp:0"),
        callee_name: callee.to_string(),
        callee_file: Some("test.cpp".to_string()),
        is_dynamic: false,
        call_line: 10,
    }
}

#[test]
fn test_empty_graph() {
    let cg = CallGraph::new();
    assert_eq!(cg.node_count(), 0);
    assert_eq!(cg.edge_count(), 0);
}

#[test]
fn test_ingest_single_edge() {
    let mut cg = CallGraph::new();
    let edges = vec![make_edge("main", "foo")];
    cg.ingest_edges(&edges);

    assert_eq!(cg.node_count(), 2);
    assert_eq!(cg.edge_count(), 1);

    let callees = cg.direct_callees("main@test.cpp:0");
    assert_eq!(callees.len(), 1);
    assert_eq!(callees[0].display_name, "foo");
}

#[test]
fn test_ingest_deduplicates_edges() {
    let mut cg = CallGraph::new();
    let edges = vec![
        make_edge("main", "foo"),
        make_edge("main", "foo"), // duplicate
    ];
    cg.ingest_edges(&edges);

    assert_eq!(cg.node_count(), 2);
    assert_eq!(cg.edge_count(), 1); // deduplicated
}

#[test]
fn test_deduplicates_nodes_by_usr() {
    let mut cg = CallGraph::new();
    let edges = vec![make_edge("main", "foo"), make_edge("foo", "bar")];
    cg.ingest_edges(&edges);

    // "foo" appears as both callee (from main) and caller (to bar)
    // but should be one node.
    assert_eq!(cg.node_count(), 3); // main, foo, bar
    assert_eq!(cg.edge_count(), 2);
}

#[test]
fn test_dfs_callees() {
    let mut cg = CallGraph::new();
    // main -> foo -> bar -> baz
    //              -> qux
    let edges = vec![
        make_edge("main", "foo"),
        make_edge("foo", "bar"),
        make_edge("foo", "qux"),
        make_edge("bar", "baz"),
    ];
    cg.ingest_edges(&edges);

    let result = cg.callees_dfs("main@test.cpp:0", None);
    let names: Vec<&str> = result.iter().map(|n| n.display_name.as_str()).collect();

    // DFS preorder starting from main should visit all reachable nodes.
    assert!(names.contains(&"main"));
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"bar"));
    assert!(names.contains(&"baz"));
    assert!(names.contains(&"qux"));
    assert_eq!(names.len(), 5);

    // main should be first (depth 0).
    assert_eq!(result[0].display_name, "main");
    assert_eq!(result[0].depth, 0);
}

#[test]
fn test_bfs_callees() {
    let mut cg = CallGraph::new();
    // main -> foo -> baz
    // main -> bar -> baz
    let edges = vec![
        make_edge("main", "foo"),
        make_edge("main", "bar"),
        make_edge("foo", "baz"),
        make_edge("bar", "baz"),
    ];
    cg.ingest_edges(&edges);

    let result = cg.callees_bfs("main@test.cpp:0", None);
    let names: Vec<&str> = result.iter().map(|n| n.display_name.as_str()).collect();

    assert_eq!(result[0].display_name, "main");
    assert_eq!(result[0].depth, 0);

    // foo and bar should be at depth 1 (direct callees of main).
    let depth_1: Vec<&str> = result
        .iter()
        .filter(|n| n.depth == 1)
        .map(|n| n.display_name.as_str())
        .collect();
    assert!(depth_1.contains(&"foo") || depth_1.contains(&"bar"));

    // baz at depth 2.
    let baz_nodes: Vec<_> = result.iter().filter(|n| n.display_name == "baz").collect();
    assert!(!baz_nodes.is_empty());
    assert_eq!(baz_nodes[0].depth, 2);
}

#[test]
fn test_max_depth_limits_traversal() {
    let mut cg = CallGraph::new();
    // main -> foo -> bar -> baz -> qux
    let edges = vec![
        make_edge("main", "foo"),
        make_edge("foo", "bar"),
        make_edge("bar", "baz"),
        make_edge("baz", "qux"),
    ];
    cg.ingest_edges(&edges);

    let result = cg.callees_bfs("main@test.cpp:0", Some(2));
    let names: Vec<&str> = result.iter().map(|n| n.display_name.as_str()).collect();

    // Should include main (0), foo (1), bar (2) but NOT baz (3) or qux (4).
    assert!(names.contains(&"main"));
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"bar"));
    assert!(!names.contains(&"baz"));
    assert!(!names.contains(&"qux"));
}

#[test]
fn test_callers_reverse_traversal() {
    let mut cg = CallGraph::new();
    // main -> foo
    // helper -> foo
    // foo -> bar
    let edges = vec![
        make_edge("main", "foo"),
        make_edge("helper", "foo"),
        make_edge("foo", "bar"),
    ];
    cg.ingest_edges(&edges);

    let callers = cg.callers_bfs("foo@test.cpp:0", None);
    let names: Vec<&str> = callers.iter().map(|n| n.display_name.as_str()).collect();

    // foo itself at depth 0, then main and helper at depth 1.
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"main"));
    assert!(names.contains(&"helper"));
    // bar should NOT appear (it's a callee, not a caller).
    assert!(!names.contains(&"bar"));
}

#[test]
fn test_direct_callers_and_callees() {
    let mut cg = CallGraph::new();
    let edges = vec![
        make_edge("main", "foo"),
        make_edge("helper", "foo"),
        make_edge("foo", "bar"),
        make_edge("foo", "baz"),
    ];
    cg.ingest_edges(&edges);

    let direct_callees: Vec<&str> = cg
        .direct_callees("foo@test.cpp:0")
        .iter()
        .map(|n| n.display_name.as_str())
        .collect();
    assert!(direct_callees.contains(&"bar"));
    assert!(direct_callees.contains(&"baz"));
    assert_eq!(direct_callees.len(), 2);

    let direct_callers: Vec<&str> = cg
        .direct_callers("foo@test.cpp:0")
        .iter()
        .map(|n| n.display_name.as_str())
        .collect();
    assert!(direct_callers.contains(&"main"));
    assert!(direct_callers.contains(&"helper"));
    assert_eq!(direct_callers.len(), 2);
}

#[test]
fn test_find_by_name() {
    let mut cg = CallGraph::new();
    let edges = vec![
        make_edge("MyClass::process", "MyClass::validate"),
        make_edge("MyClass::process", "helper_func"),
    ];
    cg.ingest_edges(&edges);

    let matches = cg.find_by_name("MyClass");
    assert_eq!(matches.len(), 2); // process and validate

    let matches = cg.find_by_name("helper");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].display_name, "helper_func");
}

#[test]
fn test_cycle_handling() {
    let mut cg = CallGraph::new();
    // foo -> bar -> baz -> foo (cycle)
    let edges = vec![
        make_edge("foo", "bar"),
        make_edge("bar", "baz"),
        make_edge("baz", "foo"),
    ];
    cg.ingest_edges(&edges);

    // DFS should still terminate and visit each node exactly once.
    let result = cg.callees_dfs("foo@test.cpp:0", None);
    assert_eq!(result.len(), 3);
    let names: Vec<&str> = result.iter().map(|n| n.display_name.as_str()).collect();
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"bar"));
    assert!(names.contains(&"baz"));
}

#[test]
fn test_nonexistent_start_node() {
    let cg = CallGraph::new();
    let result = cg.callees_dfs("nonexistent@test.cpp:0", None);
    assert!(result.is_empty());
}

#[test]
fn test_edge_cache_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let cache_dir = dir.path();

    let cached = CachedEdges {
        file: "src/main.cpp".to_string(),
        file_mtime_secs: 1234567890,
        compile_args_hash: 42,
        edges: vec![SerializableEdge {
            caller_usr: "main@main.cpp:1".to_string(),
            caller_name: "main".to_string(),
            caller_file: "main.cpp".to_string(),
            callee_usr: "foo@foo.cpp:10".to_string(),
            callee_name: "foo".to_string(),
            callee_file: Some("foo.cpp".to_string()),
            is_dynamic: false,
            call_line: 5,
        }],
    };

    save_edge_cache(cache_dir, "src/main.cpp", &cached).unwrap();
    let loaded = load_edge_cache(cache_dir, "src/main.cpp").unwrap();

    assert_eq!(loaded.file, "src/main.cpp");
    assert_eq!(loaded.file_mtime_secs, 1234567890);
    assert_eq!(loaded.compile_args_hash, 42);
    assert_eq!(loaded.edges.len(), 1);
    assert_eq!(loaded.edges[0].caller_name, "main");
    assert_eq!(loaded.edges[0].callee_name, "foo");
}

#[test]
fn test_dynamic_call_edge() {
    let mut cg = CallGraph::new();
    let edges = vec![CallEdge {
        caller_usr: "main@test.cpp:0".to_string(),
        caller_name: "main".to_string(),
        caller_file: "test.cpp".to_string(),
        callee_usr: "Base::vtable_method@base.h:10".to_string(),
        callee_name: "Base::vtable_method".to_string(),
        callee_file: Some("base.h".to_string()),
        is_dynamic: true,
        call_line: 20,
    }];
    cg.ingest_edges(&edges);

    assert_eq!(cg.node_count(), 2);
    assert_eq!(cg.edge_count(), 1);

    let callees = cg.direct_callees("main@test.cpp:0");
    assert_eq!(callees[0].display_name, "Base::vtable_method");
}

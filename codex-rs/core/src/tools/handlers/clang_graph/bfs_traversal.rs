//! Deterministic depth-limited BFS call graph traversal.
//!
//! Combines manifest-backed ripgrep search (for high recall) with optional
//! libclang validation (for high precision) to build a call graph radiating
//! outward from a root symbol.
//!
//! The traversal is callers-first by default (blast radius analysis), then
//! fans out into callees.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;

use serde::Serialize;

use super::clang_engine::ClangEngine;
use super::graph::FunctionNode;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Controls BFS traversal behavior.
pub struct BfsConfig {
    /// Maximum traversal depth (0 = root only, 1 = direct neighbors, 2 = two hops).
    pub max_depth: u32,
    /// Stop adding nodes once this count is reached.
    pub max_nodes: usize,
    /// Stop adding edges once this count is reached.
    pub max_edges: usize,
    /// Max callers to discover per hop (via rg + validate).
    pub max_callers_per_hop: usize,
    /// Max callees to discover per hop.
    pub max_callees_per_hop: usize,
    /// Whether callers are explored before callees at each depth.
    pub prioritize: BfsPriority,
}

impl Default for BfsConfig {
    fn default() -> Self {
        Self {
            max_depth: 2,
            max_nodes: 200,
            max_edges: 500,
            max_callers_per_hop: 15,
            max_callees_per_hop: 20,
            prioritize: BfsPriority::default(),
        }
    }
}

/// Which direction to explore first at each BFS depth.
#[derive(Default)]
pub enum BfsPriority {
    /// Explore callers (incoming edges / blast radius) first.
    #[default]
    CallersFirst,
    /// Explore callees (outgoing edges) first.
    CalleesFirst,
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Complete BFS result.
#[derive(Serialize, Clone, Debug)]
pub struct BfsResult {
    pub nodes: Vec<BfsNode>,
    pub edges: Vec<BfsEdge>,
    /// Deepest depth actually reached by the traversal.
    #[serde(rename = "maxDepthReached")]
    pub max_depth_reached: u32,
    /// True if the traversal was stopped early due to node/edge caps.
    pub truncated: bool,
    /// "clang" or "heuristic"
    pub engine: String,
}

/// A node in the BFS graph output.
#[derive(Serialize, Clone, Debug)]
pub struct BfsNode {
    pub id: String,
    pub name: String,
    pub file: String,
    pub line: u32,
    pub depth: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usr: Option<String>,
    #[serde(rename = "isDefinition")]
    pub is_definition: bool,
}

/// An edge in the BFS graph output.
#[derive(Serialize, Clone, Debug)]
pub struct BfsEdge {
    pub from: String,
    pub to: String,
    #[serde(rename = "callType")]
    pub call_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

// ---------------------------------------------------------------------------
// BFS Engine
// ---------------------------------------------------------------------------

/// Internal node tracking during BFS.
struct QueueEntry {
    id: String,
    usr: Option<String>,
    name: String,
    file: String,
    line: u32,
    depth: u32,
    is_definition: bool,
}

/// Run a depth-limited BFS call graph traversal from a root symbol.
///
/// When `engine` is `Some`, callers/callees are extracted from the libclang
/// AST.  Otherwise, a heuristic (ripgrep) approach is used.
///
/// The `search_fn` parameter abstracts over the rg search so this module
/// doesn't depend on tokio or the specific search strategy.
pub fn bfs_call_graph(
    root_usr: Option<&str>,
    root_name: &str,
    root_file: &str,
    root_line: u32,
    engine: &mut Option<ClangEngine>,
    heuristic_callers: &[(String, u32, String)], // (file, line, context) from rg
    heuristic_callees: &[(String, Option<u32>, String)], // (callee_name, line, call_type)
    config: &BfsConfig,
) -> BfsResult {
    let mut nodes: Vec<BfsNode> = Vec::new();
    let mut edges: Vec<BfsEdge> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<QueueEntry> = VecDeque::new();
    let mut max_depth_reached: u32 = 0;
    let mut truncated = false;

    let using_clang = engine.is_some();
    let engine_label = if using_clang { "clang" } else { "heuristic" };

    // Synthesize root node ID.
    let root_id = root_usr
        .map(String::from)
        .unwrap_or_else(|| format!("{root_name}@{root_file}:{root_line}"));

    // Seed the queue.
    queue.push_back(QueueEntry {
        id: root_id.clone(),
        usr: root_usr.map(String::from),
        name: root_name.to_string(),
        file: root_file.to_string(),
        line: root_line,
        depth: 0,
        is_definition: true,
    });

    // If we have a clang engine, parse the root file to seed the graph.
    if let Some(ref mut eng) = engine {
        let _ = eng.try_parse_file(Path::new(root_file));
    }

    while let Some(entry) = queue.pop_front() {
        // Check caps.
        if nodes.len() >= config.max_nodes || edges.len() >= config.max_edges {
            truncated = true;
            break;
        }

        // Skip if already visited.
        if !visited.insert(entry.id.clone()) {
            continue;
        }

        max_depth_reached = max_depth_reached.max(entry.depth);

        // Add node.
        nodes.push(BfsNode {
            id: entry.id.clone(),
            name: entry.name.clone(),
            file: entry.file.clone(),
            line: entry.line,
            depth: entry.depth,
            usr: entry.usr.clone(),
            is_definition: entry.is_definition,
        });

        // Don't expand beyond max_depth.
        if entry.depth >= config.max_depth {
            continue;
        }

        let next_depth = entry.depth + 1;

        // Determine expansion order.
        let (first_dir, second_dir) = match config.prioritize {
            BfsPriority::CallersFirst => (Direction::Callers, Direction::Callees),
            BfsPriority::CalleesFirst => (Direction::Callees, Direction::Callers),
        };

        for dir in [first_dir, second_dir] {
            if nodes.len() >= config.max_nodes || edges.len() >= config.max_edges {
                truncated = true;
                break;
            }

            match dir {
                Direction::Callers => {
                    let callers = discover_callers(
                        &entry,
                        engine,
                        heuristic_callers,
                        config.max_callers_per_hop,
                    );
                    for caller in callers {
                        if edges.len() >= config.max_edges {
                            truncated = true;
                            break;
                        }
                        edges.push(BfsEdge {
                            from: caller.id.clone(),
                            to: entry.id.clone(),
                            call_type: caller.call_type.clone(),
                            file: Some(caller.file.clone()),
                            line: Some(caller.line),
                        });
                        if !visited.contains(&caller.id) {
                            queue.push_back(QueueEntry {
                                id: caller.id,
                                usr: caller.usr,
                                name: caller.name,
                                file: caller.file,
                                line: caller.line,
                                depth: next_depth,
                                is_definition: caller.is_definition,
                            });
                        }
                    }
                }
                Direction::Callees => {
                    let callees = discover_callees(
                        &entry,
                        engine,
                        heuristic_callees,
                        config.max_callees_per_hop,
                    );
                    for callee in callees {
                        if edges.len() >= config.max_edges {
                            truncated = true;
                            break;
                        }
                        edges.push(BfsEdge {
                            from: entry.id.clone(),
                            to: callee.id.clone(),
                            call_type: callee.call_type.clone(),
                            file: if callee.file.is_empty() {
                                None
                            } else {
                                Some(callee.file.clone())
                            },
                            line: if callee.line > 0 {
                                Some(callee.line)
                            } else {
                                None
                            },
                        });
                        if !visited.contains(&callee.id) {
                            queue.push_back(QueueEntry {
                                id: callee.id,
                                usr: callee.usr,
                                name: callee.name,
                                file: callee.file,
                                line: callee.line,
                                depth: next_depth,
                                is_definition: callee.is_definition,
                            });
                        }
                    }
                }
            }
        }
    }

    BfsResult {
        nodes,
        edges,
        max_depth_reached,
        truncated,
        engine: engine_label.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Direction {
    Callers,
    Callees,
}

struct DiscoveredNode {
    id: String,
    usr: Option<String>,
    name: String,
    file: String,
    line: u32,
    call_type: String,
    is_definition: bool,
}

/// Discover callers of a node.
///
/// Prefers clang-verified callers; falls back to heuristic rg matches.
fn discover_callers(
    target: &QueueEntry,
    engine: &mut Option<ClangEngine>,
    heuristic_callers: &[(String, u32, String)],
    max: usize,
) -> Vec<DiscoveredNode> {
    let mut results = Vec::new();

    // Try clang first.
    if let Some(ref eng) = engine {
        if let Some(ref usr) = target.usr {
            let clang_callers = eng.direct_callers(usr);
            for node in clang_callers.iter().take(max) {
                results.push(DiscoveredNode {
                    id: node.usr.clone(),
                    usr: Some(node.usr.clone()),
                    name: node.display_name.clone(),
                    file: node.file.clone(),
                    line: node.line,
                    call_type: "ast".to_string(),
                    is_definition: node.is_definition,
                });
            }
        }
    }

    // If clang didn't find enough, supplement with heuristic callers.
    // Only use heuristics for the root node (depth 0).
    if results.len() < max && target.depth == 0 {
        let seen: HashSet<String> = results
            .iter()
            .map(|n| format!("{}:{}", n.file, n.line))
            .collect();

        for (file, line, context) in heuristic_callers.iter().take(max * 2) {
            let key = format!("{file}:{line}");
            if seen.contains(&key) {
                continue;
            }
            let id = format!("heuristic@{file}:{line}");
            results.push(DiscoveredNode {
                id,
                usr: None,
                name: context.trim().to_string(),
                file: file.clone(),
                line: *line,
                call_type: "heuristic".to_string(),
                is_definition: false,
            });
            if results.len() >= max {
                break;
            }
        }
    }

    results.truncate(max);
    results
}

/// Discover callees of a node.
///
/// Prefers clang-verified callees; falls back to heuristic text extraction.
fn discover_callees(
    source: &QueueEntry,
    engine: &mut Option<ClangEngine>,
    heuristic_callees: &[(String, Option<u32>, String)],
    max: usize,
) -> Vec<DiscoveredNode> {
    let mut results = Vec::new();

    // Try clang first.
    if let Some(ref eng) = engine {
        if let Some(ref usr) = source.usr {
            let clang_callees = eng.direct_callees(usr);
            for node in clang_callees.iter().take(max) {
                results.push(DiscoveredNode {
                    id: node.usr.clone(),
                    usr: Some(node.usr.clone()),
                    name: node.display_name.clone(),
                    file: node.file.clone(),
                    line: node.line,
                    call_type: "ast".to_string(),
                    is_definition: node.is_definition,
                });
            }
        }
    }

    // Supplement with heuristic callees if needed.
    if results.len() < max && source.depth == 0 {
        let seen: HashSet<String> = results.iter().map(|n| n.name.clone()).collect();

        for (callee_name, line, call_type) in heuristic_callees.iter().take(max * 2) {
            if seen.contains(callee_name) {
                continue;
            }
            let id = format!("heuristic:{callee_name}");
            results.push(DiscoveredNode {
                id,
                usr: None,
                name: callee_name.clone(),
                file: String::new(),
                line: line.unwrap_or(0),
                call_type: format!("heuristic:{call_type}"),
                is_definition: false,
            });
            if results.len() >= max {
                break;
            }
        }
    }

    results.truncate(max);
    results
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bfs_empty_depth_zero() {
        let config = BfsConfig {
            max_depth: 0,
            ..Default::default()
        };
        let result = bfs_call_graph(
            Some("main@test.cpp:1"),
            "main",
            "test.cpp",
            1,
            &mut None,
            &[],
            &[],
            &config,
        );
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.edges.len(), 0);
        assert_eq!(result.nodes[0].name, "main");
        assert_eq!(result.engine, "heuristic");
        assert!(!result.truncated);
    }

    #[test]
    fn test_bfs_heuristic_callers() {
        let config = BfsConfig {
            max_depth: 1,
            max_callers_per_hop: 3,
            max_callees_per_hop: 0,
            ..Default::default()
        };
        let callers = vec![
            ("caller1.cpp".to_string(), 10, "caller1();".to_string()),
            ("caller2.cpp".to_string(), 20, "caller2();".to_string()),
        ];
        let result = bfs_call_graph(
            Some("target@test.cpp:1"),
            "target",
            "test.cpp",
            1,
            &mut None,
            &callers,
            &[],
            &config,
        );
        // Root + 2 callers = 3 nodes
        assert_eq!(result.nodes.len(), 3);
        // 2 caller edges
        assert_eq!(result.edges.len(), 2);
        assert_eq!(result.max_depth_reached, 1);
    }

    #[test]
    fn test_bfs_heuristic_callees() {
        let config = BfsConfig {
            max_depth: 1,
            max_callers_per_hop: 0,
            max_callees_per_hop: 3,
            ..Default::default()
        };
        let callees = vec![
            ("callee1".to_string(), Some(5), "function".to_string()),
            ("callee2".to_string(), None, "method".to_string()),
        ];
        let result = bfs_call_graph(
            Some("root@test.cpp:1"),
            "root",
            "test.cpp",
            1,
            &mut None,
            &[],
            &callees,
            &config,
        );
        assert_eq!(result.nodes.len(), 3); // root + 2 callees
        assert_eq!(result.edges.len(), 2);
    }

    #[test]
    fn test_bfs_node_cap_truncation() {
        let config = BfsConfig {
            max_depth: 1,
            max_nodes: 2,
            max_edges: 100,
            max_callers_per_hop: 5,
            max_callees_per_hop: 5,
            ..Default::default()
        };
        let callers = vec![
            ("a.cpp".to_string(), 1, "a();".to_string()),
            ("b.cpp".to_string(), 2, "b();".to_string()),
            ("c.cpp".to_string(), 3, "c();".to_string()),
        ];
        let result = bfs_call_graph(
            Some("root@test.cpp:1"),
            "root",
            "test.cpp",
            1,
            &mut None,
            &callers,
            &[],
            &config,
        );
        assert!(result.nodes.len() <= 2);
        assert!(result.truncated);
    }

    #[test]
    fn test_bfs_deduplication() {
        let config = BfsConfig {
            max_depth: 1,
            ..Default::default()
        };
        // Same caller appearing twice should be deduped.
        let callers = vec![
            ("same.cpp".to_string(), 10, "call();".to_string()),
            ("same.cpp".to_string(), 10, "call();".to_string()),
        ];
        let result = bfs_call_graph(
            Some("root@test.cpp:1"),
            "root",
            "test.cpp",
            1,
            &mut None,
            &callers,
            &[],
            &config,
        );
        // Root + 1 unique caller = 2 (not 3)
        assert_eq!(result.nodes.len(), 2);
    }
}

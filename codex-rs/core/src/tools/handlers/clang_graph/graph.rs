//! Call graph data structure backed by petgraph with DFS/BFS traversal.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;

use petgraph::Direction;
use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;
use petgraph::visit::Bfs;
use petgraph::visit::Dfs;
use petgraph::visit::DfsPostOrder;
use serde::Deserialize;
use serde::Serialize;

use super::edge_extractor::CallEdge;
use super::edge_extractor::FunctionInfo;

/// Metadata stored per node in the call graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionNode {
    /// USR or synthetic unique identifier.
    pub usr: String,
    /// Human-readable display name (e.g., `MyClass::foo(int, int)`).
    pub display_name: String,
    /// Primary file where this function is defined.
    pub file: String,
    /// Line number of definition.
    pub line: u32,
    /// Whether this is a confirmed definition (vs. just a declaration).
    pub is_definition: bool,
}

/// Controls the order of graph traversal results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalOrder {
    /// Depth-first, preorder (discover nodes as they're first visited).
    DfsPreorder,
    /// Depth-first, postorder (visit callees before callers).
    DfsPostorder,
    /// Breadth-first (level by level fan-out).
    Bfs,
}

/// The core call graph: a directed graph where nodes are functions and
/// edges are call relationships.
///
/// Nodes are keyed by USR for cross-TU deduplication. Edges are
/// deduplicated by (caller_usr, callee_usr).
#[derive(Debug)]
pub struct CallGraph {
    graph: DiGraph<FunctionNode, EdgeMeta>,
    /// USR → NodeIndex lookup for O(1) node resolution.
    node_map: HashMap<String, NodeIndex>,
}

/// Metadata on a call edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeMeta {
    /// Whether this call is through virtual/dynamic dispatch.
    pub is_dynamic: bool,
    /// Source file where the call occurs.
    pub call_file: String,
    /// Line number of the call site.
    pub call_line: u32,
}

/// A node in a traversal result.
#[derive(Debug, Clone)]
pub struct TraversalNode {
    pub usr: String,
    pub display_name: String,
    pub file: String,
    pub line: u32,
    /// Depth from the start node (0 = start node itself).
    pub depth: u32,
}

impl CallGraph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            node_map: HashMap::new(),
        }
    }

    /// Number of function nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of call edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Get or insert a function node by USR.
    fn get_or_insert_node(
        &mut self,
        usr: &str,
        default: impl FnOnce() -> FunctionNode,
    ) -> NodeIndex {
        if let Some(&idx) = self.node_map.get(usr) {
            return idx;
        }
        let node = default();
        let idx = self.graph.add_node(node);
        self.node_map.insert(usr.to_string(), idx);
        idx
    }

    /// Ingest a batch of call edges (typically from one TU parse).
    pub fn ingest_edges(&mut self, edges: &[CallEdge]) {
        for edge in edges {
            let caller_idx = self.get_or_insert_node(&edge.caller_usr, || FunctionNode {
                usr: edge.caller_usr.clone(),
                display_name: edge.caller_name.clone(),
                file: edge.caller_file.clone(),
                line: 0,
                is_definition: true,
            });

            let callee_idx = self.get_or_insert_node(&edge.callee_usr, || FunctionNode {
                usr: edge.callee_usr.clone(),
                display_name: edge.callee_name.clone(),
                file: edge.callee_file.clone().unwrap_or_default(),
                line: 0,
                is_definition: false,
            });

            // Deduplicate edges.
            if self.graph.find_edge(caller_idx, callee_idx).is_none() {
                self.graph.add_edge(
                    caller_idx,
                    callee_idx,
                    EdgeMeta {
                        is_dynamic: edge.is_dynamic,
                        call_file: edge.caller_file.clone(),
                        call_line: edge.call_line,
                    },
                );
            }
        }
    }

    /// Ingest function declarations/definitions to enrich existing nodes.
    pub fn ingest_functions(&mut self, functions: &[FunctionInfo]) {
        for func in functions {
            let idx = self.get_or_insert_node(&func.usr, || FunctionNode {
                usr: func.usr.clone(),
                display_name: func.display_name.clone(),
                file: func.file.clone(),
                line: func.line,
                is_definition: func.is_definition,
            });

            // Update with richer info if this is a definition.
            if func.is_definition {
                let node = &mut self.graph[idx];
                node.file = func.file.clone();
                node.line = func.line;
                node.is_definition = true;
                if !func.display_name.is_empty() {
                    node.display_name = func.display_name.clone();
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Traversal: "who does X call?" (forward / callees)
    // -----------------------------------------------------------------------

    /// Traverse callees of a function (forward edges) in the given order.
    ///
    /// `start_usr` is the USR of the function to start from.
    /// `max_depth` limits traversal depth (0 = just the start node).
    pub fn callees(
        &self,
        start_usr: &str,
        order: TraversalOrder,
        max_depth: Option<u32>,
    ) -> Vec<TraversalNode> {
        let Some(&start) = self.node_map.get(start_usr) else {
            return vec![];
        };
        self.traverse_directed(start, Direction::Outgoing, order, max_depth)
    }

    /// Find all functions called by `start_usr`, convenience for DFS preorder.
    pub fn callees_dfs(&self, start_usr: &str, max_depth: Option<u32>) -> Vec<TraversalNode> {
        self.callees(start_usr, TraversalOrder::DfsPreorder, max_depth)
    }

    /// Find all functions called by `start_usr`, BFS (level-order).
    pub fn callees_bfs(&self, start_usr: &str, max_depth: Option<u32>) -> Vec<TraversalNode> {
        self.callees(start_usr, TraversalOrder::Bfs, max_depth)
    }

    // -----------------------------------------------------------------------
    // Traversal: "who calls X?" (reverse / callers)
    // -----------------------------------------------------------------------

    /// Traverse callers of a function (reverse edges) in the given order.
    pub fn callers(
        &self,
        target_usr: &str,
        order: TraversalOrder,
        max_depth: Option<u32>,
    ) -> Vec<TraversalNode> {
        let Some(&target) = self.node_map.get(target_usr) else {
            return vec![];
        };
        self.traverse_directed(target, Direction::Incoming, order, max_depth)
    }

    /// Find all callers of `target_usr`, DFS preorder.
    pub fn callers_dfs(&self, target_usr: &str, max_depth: Option<u32>) -> Vec<TraversalNode> {
        self.callers(target_usr, TraversalOrder::DfsPreorder, max_depth)
    }

    /// Find all callers of `target_usr`, BFS.
    pub fn callers_bfs(&self, target_usr: &str, max_depth: Option<u32>) -> Vec<TraversalNode> {
        self.callers(target_usr, TraversalOrder::Bfs, max_depth)
    }

    // -----------------------------------------------------------------------
    // Lookup
    // -----------------------------------------------------------------------

    /// Look up a function node by USR.
    pub fn get_node(&self, usr: &str) -> Option<&FunctionNode> {
        self.node_map.get(usr).map(|&idx| &self.graph[idx])
    }

    /// Find nodes whose display name contains the given substring.
    pub fn find_by_name(&self, name_substring: &str) -> Vec<&FunctionNode> {
        self.graph
            .node_weights()
            .filter(|n| n.display_name.contains(name_substring))
            .collect()
    }

    /// Direct callees of a function (non-transitive, one hop).
    pub fn direct_callees(&self, usr: &str) -> Vec<&FunctionNode> {
        let Some(&idx) = self.node_map.get(usr) else {
            return vec![];
        };
        self.graph
            .neighbors_directed(idx, Direction::Outgoing)
            .map(|n| &self.graph[n])
            .collect()
    }

    /// Direct callers of a function (non-transitive, one hop).
    pub fn direct_callers(&self, usr: &str) -> Vec<&FunctionNode> {
        let Some(&idx) = self.node_map.get(usr) else {
            return vec![];
        };
        self.graph
            .neighbors_directed(idx, Direction::Incoming)
            .map(|n| &self.graph[n])
            .collect()
    }

    // -----------------------------------------------------------------------
    // Internal traversal engine
    // -----------------------------------------------------------------------

    fn traverse_directed(
        &self,
        start: NodeIndex,
        direction: Direction,
        order: TraversalOrder,
        max_depth: Option<u32>,
    ) -> Vec<TraversalNode> {
        // For directional traversal we need a view of the graph that
        // follows edges in the requested direction. For callers (Incoming),
        // we traverse the reversed graph.
        //
        // petgraph's Dfs/Bfs follow outgoing edges by default, so for
        // Incoming traversal we use the reversed graph.

        let mut results = Vec::new();

        match direction {
            Direction::Outgoing => {
                self.traverse_outgoing(start, order, max_depth, &mut results);
            }
            Direction::Incoming => {
                // Build a reversed view for incoming traversal.
                self.traverse_incoming(start, order, max_depth, &mut results);
            }
        }

        results
    }

    fn traverse_outgoing(
        &self,
        start: NodeIndex,
        order: TraversalOrder,
        max_depth: Option<u32>,
        results: &mut Vec<TraversalNode>,
    ) {
        match order {
            TraversalOrder::DfsPreorder => {
                let mut dfs = Dfs::new(&self.graph, start);
                let mut depth_map: HashMap<NodeIndex, u32> = HashMap::new();
                depth_map.insert(start, 0);
                while let Some(node) = dfs.next(&self.graph) {
                    let depth = *depth_map.get(&node).unwrap_or(&0);
                    if let Some(max) = max_depth {
                        if depth > max {
                            continue;
                        }
                    }
                    let fn_node = &self.graph[node];
                    results.push(TraversalNode {
                        usr: fn_node.usr.clone(),
                        display_name: fn_node.display_name.clone(),
                        file: fn_node.file.clone(),
                        line: fn_node.line,
                        depth,
                    });
                    // Set depth for neighbors.
                    for neighbor in self.graph.neighbors_directed(node, Direction::Outgoing) {
                        depth_map.entry(neighbor).or_insert(depth + 1);
                    }
                }
            }
            TraversalOrder::DfsPostorder => {
                let mut dfs = DfsPostOrder::new(&self.graph, start);
                while let Some(node) = dfs.next(&self.graph) {
                    let fn_node = &self.graph[node];
                    results.push(TraversalNode {
                        usr: fn_node.usr.clone(),
                        display_name: fn_node.display_name.clone(),
                        file: fn_node.file.clone(),
                        line: fn_node.line,
                        depth: 0, // postorder depth is complex, skip for now
                    });
                }
            }
            TraversalOrder::Bfs => {
                let mut bfs = Bfs::new(&self.graph, start);
                let mut depth_map: HashMap<NodeIndex, u32> = HashMap::new();
                depth_map.insert(start, 0);
                while let Some(node) = bfs.next(&self.graph) {
                    let depth = *depth_map.get(&node).unwrap_or(&0);
                    if let Some(max) = max_depth {
                        if depth > max {
                            continue;
                        }
                    }
                    let fn_node = &self.graph[node];
                    results.push(TraversalNode {
                        usr: fn_node.usr.clone(),
                        display_name: fn_node.display_name.clone(),
                        file: fn_node.file.clone(),
                        line: fn_node.line,
                        depth,
                    });
                    for neighbor in self.graph.neighbors_directed(node, Direction::Outgoing) {
                        depth_map.entry(neighbor).or_insert(depth + 1);
                    }
                }
            }
        }
    }

    fn traverse_incoming(
        &self,
        start: NodeIndex,
        order: TraversalOrder,
        max_depth: Option<u32>,
        results: &mut Vec<TraversalNode>,
    ) {
        // For incoming traversal, we manually BFS/DFS following Incoming edges.
        match order {
            TraversalOrder::Bfs | TraversalOrder::DfsPreorder => {
                let mut queue: VecDeque<(NodeIndex, u32)> = VecDeque::new();
                queue.push_back((start, 0));
                let mut visited: HashSet<NodeIndex> = HashSet::new();
                visited.insert(start);

                while let Some((node, depth)) = if order == TraversalOrder::Bfs {
                    queue.pop_front()
                } else {
                    // DFS: pop from back.
                    queue.pop_back()
                } {
                    if let Some(max) = max_depth {
                        if depth > max {
                            continue;
                        }
                    }
                    let fn_node = &self.graph[node];
                    results.push(TraversalNode {
                        usr: fn_node.usr.clone(),
                        display_name: fn_node.display_name.clone(),
                        file: fn_node.file.clone(),
                        line: fn_node.line,
                        depth,
                    });
                    for neighbor in self.graph.neighbors_directed(node, Direction::Incoming) {
                        if visited.insert(neighbor) {
                            queue.push_back((neighbor, depth + 1));
                        }
                    }
                }
            }
            TraversalOrder::DfsPostorder => {
                // Simplified: just collect in reverse DFS preorder.
                let mut pre = Vec::new();
                self.traverse_incoming(start, TraversalOrder::DfsPreorder, max_depth, &mut pre);
                pre.reverse();
                results.extend(pre);
            }
        }
    }
}

impl Default for CallGraph {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Serialization support for the edge cache
// ---------------------------------------------------------------------------

/// Serializable representation of edges for disk caching.
#[derive(Serialize, Deserialize)]
pub struct CachedEdges {
    pub file: String,
    pub file_mtime_secs: u64,
    pub compile_args_hash: u64,
    pub edges: Vec<SerializableEdge>,
}

#[derive(Serialize, Deserialize)]
pub struct SerializableEdge {
    pub caller_usr: String,
    pub caller_name: String,
    pub caller_file: String,
    pub callee_usr: String,
    pub callee_name: String,
    pub callee_file: Option<String>,
    pub is_dynamic: bool,
    pub call_line: u32,
}

impl From<&CallEdge> for SerializableEdge {
    fn from(e: &CallEdge) -> Self {
        Self {
            caller_usr: e.caller_usr.clone(),
            caller_name: e.caller_name.clone(),
            caller_file: e.caller_file.clone(),
            callee_usr: e.callee_usr.clone(),
            callee_name: e.callee_name.clone(),
            callee_file: e.callee_file.clone(),
            is_dynamic: e.is_dynamic,
            call_line: e.call_line,
        }
    }
}

impl From<&SerializableEdge> for CallEdge {
    fn from(e: &SerializableEdge) -> Self {
        Self {
            caller_usr: e.caller_usr.clone(),
            caller_name: e.caller_name.clone(),
            caller_file: e.caller_file.clone(),
            callee_usr: e.callee_usr.clone(),
            callee_name: e.callee_name.clone(),
            callee_file: e.callee_file.clone(),
            is_dynamic: e.is_dynamic,
            call_line: e.call_line,
        }
    }
}

/// Save edges for a file to disk cache.
pub fn save_edge_cache(
    cache_dir: &Path,
    file_key: &str,
    cached: &CachedEdges,
) -> Result<(), String> {
    std::fs::create_dir_all(cache_dir).map_err(|e| format!("failed to create cache dir: {e}"))?;

    let cache_file = cache_dir.join(format!("{}.bin", sanitize_filename(file_key)));
    let encoded =
        bincode::serialize(cached).map_err(|e| format!("failed to serialize edge cache: {e}"))?;
    std::fs::write(&cache_file, encoded).map_err(|e| format!("failed to write edge cache: {e}"))?;
    Ok(())
}

/// Load cached edges for a file from disk.
pub fn load_edge_cache(cache_dir: &Path, file_key: &str) -> Option<CachedEdges> {
    let cache_file = cache_dir.join(format!("{}.bin", sanitize_filename(file_key)));
    let data = std::fs::read(&cache_file).ok()?;
    bincode::deserialize(&data).ok()
}

fn sanitize_filename(s: &str) -> String {
    s.replace(['/', '\\', ':', ' '], "_")
}

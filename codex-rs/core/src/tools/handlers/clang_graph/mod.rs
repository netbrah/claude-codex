//! libclang-based call graph construction for C/C++ codebases.
//!
//! This module provides incremental, per-translation-unit call graph building
//! using libclang's AST visitor, with USR-based function identity and
//! petgraph-backed DFS/BFS traversal.
//!
//! Gated behind the `clang-graph` cargo feature.

pub mod bfs_traversal;
mod clang_engine;
mod compile_commands_index;
mod compile_db;
mod edge_extractor;
mod graph;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod integration_tests;

pub use bfs_traversal::BfsConfig;
pub use bfs_traversal::BfsEdge;
pub use bfs_traversal::BfsNode;
pub use bfs_traversal::BfsPriority;
pub use bfs_traversal::BfsResult;
pub use bfs_traversal::bfs_call_graph;
pub use clang_engine::ClangEngine;
pub use compile_commands_index::CompileArgs;
pub use compile_commands_index::CompileCommandsIndex;
pub use compile_db::CompileDbLoader;
pub use edge_extractor::CallEdge;
pub use edge_extractor::TuParser;
pub use graph::CallGraph;
pub use graph::FunctionNode;
pub use graph::TraversalOrder;

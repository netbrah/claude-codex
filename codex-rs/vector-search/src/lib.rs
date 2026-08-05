//! Hybrid BM25 + semantic vector search with pluggable chunking.
//!
//! Three layers:
//! - **Engine**: BM25 + cosine + embedding + index (chunker-agnostic)
//! - **Chunking**: `Chunker` trait with implementations for code (tree-sitter),
//!   prose (text-splitter), markdown, and C/C++ (legacy regex)
//! - **Tools**: Thin wrappers providing tool descriptions and scope policy

pub mod auto_chunker;
pub mod bm25;
pub mod chunk;
pub mod cosine;
pub mod cpp_chunker;
pub mod discover;
pub mod embed;
pub mod index;
pub mod markdown_chunker;
pub mod scope;
pub mod text_chunker;
pub mod tokenize;
pub mod tree_sitter_chunker;

pub use auto_chunker::AutoChunker;
pub use chunk::Chunk;
pub use chunk::ChunkKind;
pub use chunk::Chunker;
pub use cpp_chunker::CppFunctionChunker;
pub use index::IndexManager;
pub use index::IndexStatus;
pub use index::SearchMode;
pub use index::SearchParams;
pub use index::SearchResult;
pub use markdown_chunker::MarkdownChunker;
pub use text_chunker::TextSplitterChunker;
pub use tree_sitter_chunker::TreeSitterChunker;

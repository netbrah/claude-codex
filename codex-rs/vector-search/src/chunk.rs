//! Core chunking trait and types.
//!
//! The `Chunk` struct is the atomic unit of search — a named semantic unit
//! extracted from a file. The `Chunker` trait defines how chunks are produced.
//! Implementations are stateless; all state lives in the `IndexManager`.

/// A chunk extracted from a file — the atomic unit of search.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Chunk {
    /// Relative file path.
    pub file: String,
    /// Semantic name: function name, JSON key path, heading text, etc.
    pub symbol: String,
    /// 1-based start line (inclusive).
    pub start_line: usize,
    /// 1-based end line (inclusive).
    pub end_line: usize,
    /// Full source content of the chunk.
    pub content: String,
    /// What kind of semantic unit this represents.
    pub kind: ChunkKind,
}

/// The kind of semantic unit a chunk represents.
///
/// Informational metadata — the engine doesn't change behavior based on kind.
/// It helps the model/user understand what they're looking at in search results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChunkKind {
    /// Function or method definition.
    Function,
    /// Class, struct, or impl block.
    Class,
    /// Module or namespace.
    Module,
    /// Markdown heading section.
    Section,
    /// Text paragraph.
    Paragraph,
    /// JSON/YAML/TOML key-value pair.
    KeyValue,
    /// JSON/YAML array element.
    ArrayElement,
    /// Generic chunk (text-splitter fallback — no semantic identity).
    Generic,
}

/// Trait for extracting searchable chunks from file content.
///
/// Implementations are stateless — all state lives in the `IndexManager`.
/// A chunker produces raw chunks; the engine handles tokenization,
/// BM25 indexing, embedding, and caching.
///
/// # Design decisions
///
/// - **No `extensions()` method.** Extension dispatch is `AutoChunker`'s job.
///   A chunker shouldn't know about file types — it just processes content.
/// - **No size filtering.** The engine applies min/max content length filters
///   uniformly after chunking.
/// - **`ChunkKind` is informational, not behavioral.**
pub trait Chunker: Send + Sync {
    /// Extract chunks from file content.
    ///
    /// - `content`: raw file content as string
    /// - `file_path`: relative path (used for `Chunk.file`)
    fn chunk(&self, content: &str, file_path: &str) -> Vec<Chunk>;
}

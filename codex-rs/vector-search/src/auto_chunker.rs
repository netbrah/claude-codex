//! AutoChunker — dispatches to the appropriate chunker based on file extension.
//!
//! Resolution order:
//! 1. TreeSitterChunker for code languages (C, C++, Python, Rust, Go, Java,
//!    JavaScript, TypeScript, Bash)
//! 2. TreeSitterChunker for structured data (JSON, YAML, TOML)
//! 3. MarkdownChunker for .md/.mdx
//! 4. TextSplitterChunker for everything else (prose fallback)
//!
//! CppFunctionChunker is still exported for standalone use but is no longer
//! in the default registry — tree-sitter supersedes it.

use std::path::Path;

use crate::chunk::Chunk;
use crate::chunk::Chunker;
// CppFunctionChunker kept for backward-compat re-export; not in default registry.
#[allow(unused_imports)]
use crate::cpp_chunker::CppFunctionChunker;
use crate::markdown_chunker::MarkdownChunker;
use crate::text_chunker::TextSplitterChunker;
use crate::tree_sitter_chunker::TreeSitterChunker;

/// Extension-dispatching chunker.
///
/// Checks file extension against a registry of chunkers. First match wins.
/// Falls back to `TextSplitterChunker` for unrecognized extensions.
pub struct AutoChunker {
    registry: Vec<(Vec<String>, Box<dyn Chunker>)>,
    fallback: Box<dyn Chunker>,
}

impl AutoChunker {
    /// Create an AutoChunker with the default registry.
    pub fn new() -> Self {
        Self {
            registry: vec![
                // Code (tree-sitter AST extraction)
                (exts(&["c", "h"]), Box::new(TreeSitterChunker::c())),
                (
                    exts(&["cc", "cpp", "cxx", "hh", "hpp"]),
                    Box::new(TreeSitterChunker::cpp()),
                ),
                (exts(&["py"]), Box::new(TreeSitterChunker::python())),
                (exts(&["rs"]), Box::new(TreeSitterChunker::rust())),
                (exts(&["go"]), Box::new(TreeSitterChunker::go())),
                (exts(&["java"]), Box::new(TreeSitterChunker::java())),
                (
                    exts(&["js", "jsx"]),
                    Box::new(TreeSitterChunker::javascript()),
                ),
                (
                    exts(&["ts", "tsx"]),
                    Box::new(TreeSitterChunker::typescript()),
                ),
                (exts(&["sh", "bash"]), Box::new(TreeSitterChunker::bash())),
                // Structured data (tree-sitter key extraction)
                (exts(&["json"]), Box::new(TreeSitterChunker::json())),
                (exts(&["yaml", "yml"]), Box::new(TreeSitterChunker::yaml())),
                (exts(&["toml"]), Box::new(TreeSitterChunker::toml())),
                // Docs (text-splitter markdown)
                (exts(&["md", "mdx"]), Box::new(MarkdownChunker::new())),
            ],
            fallback: Box::new(TextSplitterChunker::new()),
        }
    }

    /// Create an AutoChunker with a custom registry.
    pub fn with_registry(
        registry: Vec<(Vec<String>, Box<dyn Chunker>)>,
        fallback: Box<dyn Chunker>,
    ) -> Self {
        Self { registry, fallback }
    }
}

impl Default for AutoChunker {
    fn default() -> Self {
        Self::new()
    }
}

impl Chunker for AutoChunker {
    fn chunk(&self, content: &str, file_path: &str) -> Vec<Chunk> {
        let ext = Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        for (extensions, chunker) in &self.registry {
            if extensions.iter().any(|e| e == ext) {
                return chunker.chunk(content, file_path);
            }
        }

        self.fallback.chunk(content, file_path)
    }
}

fn exts(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatches_cpp_to_tree_sitter() {
        let auto = AutoChunker::new();
        let src = "int add(int a, int b) {
    return a + b;
}
";
        let chunks = auto.chunk(src, "math.cc");
        // TreeSitterChunker::cpp() should extract the function definition
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(names.contains(&"add"), "should find add: {names:?}");
    }

    #[test]
    fn dispatches_c_to_tree_sitter() {
        let auto = AutoChunker::new();
        let src = "int add(int a, int b) {
    return a + b;
}
";
        let chunks = auto.chunk(src, "math.c");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(names.contains(&"add"), "should find add in .c: {names:?}");
    }

    #[test]
    fn dispatches_python_to_tree_sitter() {
        let auto = AutoChunker::new();
        let src = "def hello():\n    print('hi')\n";
        let chunks = auto.chunk(src, "app.py");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(
            names.contains(&"hello"),
            "should find hello in .py: {names:?}"
        );
    }

    #[test]
    fn dispatches_rust_to_tree_sitter() {
        let auto = AutoChunker::new();
        let src = "fn greet() {\n    println!(\"hi\");\n}\n";
        let chunks = auto.chunk(src, "lib.rs");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(
            names.contains(&"greet"),
            "should find greet in .rs: {names:?}"
        );
    }

    #[test]
    fn dispatches_markdown_to_markdown_chunker() {
        let auto = AutoChunker::new();
        let md = "# Title

Some content here.";
        let chunks = auto.chunk(md, "readme.md");
        assert!(!chunks.is_empty());
        // Should use MarkdownChunker (Section kind)
        assert!(
            chunks
                .iter()
                .any(|c| c.kind == crate::chunk::ChunkKind::Section),
            "markdown should produce Section chunks"
        );
    }

    #[test]
    fn falls_back_to_text_for_unknown() {
        let auto = AutoChunker::new();
        let txt = "Some plain text content.";
        let chunks = auto.chunk(txt, "notes.txt");
        assert!(!chunks.is_empty());
        assert!(
            chunks
                .iter()
                .all(|c| c.kind == crate::chunk::ChunkKind::Generic),
            "unknown extension should use text fallback"
        );
    }

    #[test]
    fn handles_no_extension() {
        let auto = AutoChunker::new();
        let chunks = auto.chunk("content", "Makefile");
        assert!(
            !chunks.is_empty(),
            "should still chunk files without extension"
        );
    }
}

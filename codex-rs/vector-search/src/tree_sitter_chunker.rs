//! Tree-sitter based code chunker.
//!
//! One struct, one `chunk()` implementation, query-driven per language.
//! Each language is configured with a tree-sitter query that captures
//! `@chunk` nodes and `@name` identifiers.
//!
//! # Adding a new language
//!
//! 1. Add `tree-sitter-<lang>` to Cargo.toml
//! 2. Add a constructor: `TreeSitterChunker::lang()`
//! 3. Define the query (2-5 lines of S-expression)
//! 4. Register in `AutoChunker::new()`

use tree_sitter::StreamingIterator;

use crate::chunk::Chunk;
use crate::chunk::ChunkKind;
use crate::chunk::Chunker;

/// Tree-sitter based chunker — extracts named AST nodes as chunks.
///
/// Uses tree-sitter queries with `@chunk` and `@name` captures to
/// extract semantic units (functions, classes, etc.) from source code.
pub struct TreeSitterChunker {
    language: tree_sitter::Language,
    query: tree_sitter::Query,
    default_kind: ChunkKind,
}

impl TreeSitterChunker {
    /// Create a new TreeSitterChunker with the given language, query, and default kind.
    ///
    /// The query MUST define at least one `@chunk` capture. The `@name` capture
    /// is optional — if absent, chunks are labeled "anonymous".
    ///
    /// # Panics
    ///
    /// Panics if the query is invalid for the given language.
    pub fn new(
        language: tree_sitter::Language,
        query_source: &str,
        default_kind: ChunkKind,
    ) -> Self {
        let query = tree_sitter::Query::new(&language, query_source)
            .unwrap_or_else(|e| panic!("invalid tree-sitter query: {e}"));
        Self {
            language,
            query,
            default_kind,
        }
    }

    // ── Language constructors ─────────────────────────────────────

    pub fn bash() -> Self {
        Self::new(
            tree_sitter_bash::LANGUAGE.into(),
            "(function_definition
  name: (word) @name) @chunk",
            ChunkKind::Function,
        )
    }

    pub fn c() -> Self {
        Self::new(
            tree_sitter_c::LANGUAGE.into(),
            "(function_definition
  declarator: (function_declarator
    declarator: (_) @name)) @chunk",
            ChunkKind::Function,
        )
    }

    pub fn cpp() -> Self {
        Self::new(
            tree_sitter_cpp::LANGUAGE.into(),
            "(function_definition
  declarator: (function_declarator
    declarator: (_) @name)) @chunk",
            ChunkKind::Function,
        )
    }

    pub fn go() -> Self {
        Self::new(
            tree_sitter_go::LANGUAGE.into(),
            concat!(
                "(function_declaration name: (identifier) @name) @chunk
",
                "(method_declaration name: (field_identifier) @name) @chunk
",
            ),
            ChunkKind::Function,
        )
    }

    pub fn java() -> Self {
        Self::new(
            tree_sitter_java::LANGUAGE.into(),
            concat!(
                "(method_declaration name: (identifier) @name) @chunk
",
                "(class_declaration name: (identifier) @name) @chunk
",
                "(constructor_declaration name: (identifier) @name) @chunk
",
            ),
            ChunkKind::Function,
        )
    }

    pub fn javascript() -> Self {
        Self::new(
            tree_sitter_javascript::LANGUAGE.into(),
            concat!(
                "(function_declaration name: (identifier) @name) @chunk
",
                "(class_declaration name: (identifier) @name) @chunk
",
                "(method_definition name: (property_identifier) @name) @chunk
",
                "(arrow_function) @chunk
",
            ),
            ChunkKind::Function,
        )
    }

    pub fn json() -> Self {
        Self::new(
            tree_sitter_json::LANGUAGE.into(),
            "(pair key: (string) @name) @chunk",
            ChunkKind::KeyValue,
        )
    }

    pub fn python() -> Self {
        Self::new(
            tree_sitter_python::LANGUAGE.into(),
            concat!(
                "(function_definition name: (identifier) @name) @chunk
",
                "(class_definition name: (identifier) @name) @chunk
",
            ),
            ChunkKind::Function,
        )
    }

    pub fn rust() -> Self {
        Self::new(
            tree_sitter_rust::LANGUAGE.into(),
            concat!(
                "(function_item name: (identifier) @name) @chunk
",
                "(impl_item type: (_) @name) @chunk
",
            ),
            ChunkKind::Function,
        )
    }

    pub fn toml() -> Self {
        Self::new(
            tree_sitter_toml_ng::LANGUAGE.into(),
            "(table (bare_key) @name) @chunk",
            ChunkKind::KeyValue,
        )
    }

    pub fn typescript() -> Self {
        Self::new(
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            concat!(
                "(function_declaration name: (identifier) @name) @chunk
",
                "(class_declaration name: (type_identifier) @name) @chunk
",
                "(method_definition name: (property_identifier) @name) @chunk
",
                "(arrow_function) @chunk
",
            ),
            ChunkKind::Function,
        )
    }

    pub fn yaml() -> Self {
        Self::new(
            tree_sitter_yaml::LANGUAGE.into(),
            "(block_mapping_pair
  key: (flow_node) @name) @chunk",
            ChunkKind::KeyValue,
        )
    }
}

impl Chunker for TreeSitterChunker {
    fn chunk(&self, content: &str, file_path: &str) -> Vec<Chunk> {
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&self.language).is_err() {
            tracing::warn!(file = file_path, "tree-sitter: failed to set language");
            return Vec::new();
        }

        let tree = match parser.parse(content, None) {
            Some(t) => t,
            None => {
                tracing::debug!(file = file_path, "tree-sitter: parse returned None");
                return Vec::new();
            }
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), content.as_bytes());

        let capture_names = self.query.capture_names();
        let chunk_idx = capture_names.iter().position(|n| *n == "chunk");
        let name_idx = capture_names.iter().position(|n| *n == "name");

        let lines: Vec<&str> = content.split('\n').collect();
        let mut chunks = Vec::new();

        while let Some(m) = matches.next() {
            let chunk_node: Option<tree_sitter::Node<'_>> = chunk_idx.and_then(|idx| {
                m.captures
                    .iter()
                    .find(|c| c.index as usize == idx)
                    .map(|c| c.node)
            });
            let name_node: Option<tree_sitter::Node<'_>> = name_idx.and_then(|idx| {
                m.captures
                    .iter()
                    .find(|c| c.index as usize == idx)
                    .map(|c| c.node)
            });

            if let Some(node) = chunk_node {
                let start = node.start_position().row;
                let end = node.end_position().row;
                let symbol = name_node
                    .and_then(|n: tree_sitter::Node<'_>| n.utf8_text(content.as_bytes()).ok())
                    .unwrap_or("anonymous")
                    .to_string();

                let end_clamped = end.min(lines.len().saturating_sub(1));
                let chunk_content = lines[start..=end_clamped].join("\n");

                chunks.push(Chunk {
                    file: file_path.to_string(),
                    symbol,
                    start_line: start + 1,
                    end_line: end + 1,
                    content: chunk_content,
                    kind: self.default_kind,
                });
            }
        }

        chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_extracts_functions() {
        let chunker = TreeSitterChunker::c();
        let src = r#"
#include <stdio.h>

int add(int a, int b) {
    return a + b;
}

void greet(const char *name) {
    printf("Hello, %s
", name);
}
"#;
        let chunks = chunker.chunk(src, "math.c");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(names.contains(&"add"), "should find add: {names:?}");
        assert!(names.contains(&"greet"), "should find greet: {names:?}");
        for c in &chunks {
            assert_eq!(c.kind, ChunkKind::Function);
        }
    }

    #[test]
    fn cpp_extracts_functions() {
        let chunker = TreeSitterChunker::cpp();
        let src = r#"
int multiply(int a, int b) {
    return a * b;
}

namespace ns {
    void helper() {
        // body
    }
}
"#;
        let chunks = chunker.chunk(src, "lib.cpp");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(
            names.contains(&"multiply"),
            "should find multiply: {names:?}"
        );
    }

    #[test]
    fn python_extracts_functions_and_classes() {
        let chunker = TreeSitterChunker::python();
        let src = r#"
def hello(name):
    print(f"Hello {name}")

class Calculator:
    def add(self, a, b):
        return a + b
"#;
        let chunks = chunker.chunk(src, "app.py");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(names.contains(&"hello"), "should find hello: {names:?}");
        assert!(
            names.contains(&"Calculator"),
            "should find Calculator: {names:?}"
        );
    }

    #[test]
    fn rust_extracts_functions_and_impls() {
        let chunker = TreeSitterChunker::rust();
        let src = r#"
fn main() {
    println!("hello");
}

impl Foo {
    fn bar(&self) -> i32 {
        42
    }
}
"#;
        let chunks = chunker.chunk(src, "main.rs");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(names.contains(&"main"), "should find main: {names:?}");
    }

    #[test]
    fn go_extracts_functions() {
        let chunker = TreeSitterChunker::go();
        let src = r#"
package main

func Add(a, b int) int {
    return a + b
}

func (s *Server) Start() error {
    return nil
}
"#;
        let chunks = chunker.chunk(src, "main.go");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(names.contains(&"Add"), "should find Add: {names:?}");
        assert!(names.contains(&"Start"), "should find Start: {names:?}");
    }

    #[test]
    fn json_extracts_keys() {
        let chunker = TreeSitterChunker::json();
        let src = r#"{
  "name": "test-project",
  "version": "1.0.0",
  "dependencies": {
    "lodash": "4.17.21"
  }
}"#;
        let chunks = chunker.chunk(src, "package.json");
        assert!(!chunks.is_empty(), "should find JSON keys");
        for c in &chunks {
            assert_eq!(c.kind, ChunkKind::KeyValue);
        }
    }

    #[test]
    fn javascript_extracts_functions() {
        let chunker = TreeSitterChunker::javascript();
        let src = r#"
function greet(name) {
    return `Hello ${name}`;
}

class Greeter {
    constructor(prefix) {
        this.prefix = prefix;
    }

    greet(name) {
        return `${this.prefix} ${name}`;
    }
}
"#;
        let chunks = chunker.chunk(src, "app.js");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(names.contains(&"greet"), "should find greet: {names:?}");
        assert!(names.contains(&"Greeter"), "should find Greeter: {names:?}");
    }

    #[test]
    fn bash_extracts_functions() {
        let chunker = TreeSitterChunker::bash();
        let src = r#"
#!/bin/bash

greet() {
    echo "Hello $1"
}

cleanup() {
    rm -rf /tmp/test
}
"#;
        let chunks = chunker.chunk(src, "deploy.sh");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(names.contains(&"greet"), "should find greet: {names:?}");
        assert!(names.contains(&"cleanup"), "should find cleanup: {names:?}");
    }

    #[test]
    fn empty_content_returns_empty() {
        let chunker = TreeSitterChunker::c();
        let chunks = chunker.chunk("", "empty.c");
        assert!(chunks.is_empty());
    }

    #[test]
    fn invalid_content_returns_empty_gracefully() {
        let chunker = TreeSitterChunker::c();
        // Should not panic — tree-sitter is error-tolerant
        let _chunks = chunker.chunk("this is not valid C code {{{", "bad.c");
    }
}

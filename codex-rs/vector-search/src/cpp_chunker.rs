//! C/C++ function-level chunker — legacy regex state machine.
//!
//! Port of opencode's `chunk()` — a line-by-line state machine that
//! extracts top-level function definitions from C/C++ source files.
//! Implements the `Chunker` trait for pluggable use with `IndexManager`.

use regex::Regex;
use std::sync::LazyLock;

use crate::chunk::Chunk;
use crate::chunk::ChunkKind;
use crate::chunk::Chunker;

static FUNC_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\w+(?:::\w+)*)\s*\(").expect("valid regex"));

static SKIP_KEYWORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^\s*(namespace|class|struct|enum|union|if|for|while|switch|do|try|catch|extern\s+"C")\b"#,
    )
    .expect("valid regex")
});

static ALL_CAPS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z_][A-Z_0-9]*$").expect("valid regex"));

/// C/C++ function-level chunker using regex-based parsing.
///
/// Only considers top-level definitions (indent ≤ 4 spaces). Skips
/// `struct`, `class`, `namespace`, macros (ALL_CAPS names), and
/// functions outside the 30–15,000 character range.
pub struct CppFunctionChunker;

impl CppFunctionChunker {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CppFunctionChunker {
    fn default() -> Self {
        Self::new()
    }
}

impl Chunker for CppFunctionChunker {
    fn chunk(&self, source: &str, file: &str) -> Vec<Chunk> {
        let lines: Vec<&str> = source.split('\n').collect();
        let mut result = Vec::new();
        let mut i: usize = 0;

        while i < lines.len() {
            let raw = lines[i];
            let trimmed = raw.trim_start();

            // Skip empty, preprocessor, comments
            if trimmed.is_empty()
                || trimmed.starts_with('#')
                || trimmed.starts_with("//")
                || trimmed.starts_with("/*")
            {
                i += 1;
                continue;
            }

            // Only top-level or near-top-level (indent ≤ 4)
            let indent = raw.len() - trimmed.len();
            if indent > 4 {
                i += 1;
                continue;
            }

            // Accumulate signature lines until `{` or `;` (max 8 lines lookahead)
            let mut sig = String::new();
            let mut j = i;
            while j < lines.len() && j < i + 8 {
                sig.push_str(lines[j]);
                sig.push('\n');
                if sig.contains('{') || sig.trim_end().ends_with(';') {
                    break;
                }
                j += 1;
            }

            // Must have `(` and `{`, must not end with `;`
            if !sig.contains('(') || !sig.contains('{') || sig.trim_end().ends_with(';') {
                i += 1;
                continue;
            }

            // Skip aggregate types and control flow
            if SKIP_KEYWORD_RE.is_match(&sig) {
                i += 1;
                continue;
            }

            // Extract function/method name
            let name = match FUNC_NAME_RE.captures(&sig) {
                Some(caps) => caps[1].to_string(),
                None => {
                    i += 1;
                    continue;
                }
            };

            // Skip ALL_CAPS macro names
            if ALL_CAPS_RE.is_match(&name) {
                i += 1;
                continue;
            }

            // Find opening brace line
            let mut brace = i;
            while brace <= j && !lines[brace].contains('{') {
                brace += 1;
            }

            // Track brace depth to find closing `}`
            let mut depth: i32 = 0;
            let mut k = brace;
            let mut closed = false;
            while k < lines.len() {
                for ch in lines[k].chars() {
                    if ch == '{' {
                        depth += 1;
                    }
                    if ch == '}' {
                        depth -= 1;
                        if depth == 0 {
                            closed = true;
                            break;
                        }
                    }
                }
                if closed {
                    break;
                }
                k += 1;
            }

            if !closed {
                i += 1;
                continue;
            }

            // Include preceding doc comments (up to 10 lines)
            let mut start = i;
            while start > 0 && start > i.saturating_sub(10) {
                let prev = lines[start - 1].trim();
                if prev.starts_with("//")
                    || prev.starts_with('*')
                    || prev.starts_with("/*")
                    || prev.ends_with("*/")
                {
                    start -= 1;
                } else {
                    break;
                }
            }

            let content: String = lines[start..=k].join("\n");
            if content.len() >= 30 && content.len() <= 15_000 {
                result.push(Chunk {
                    file: file.to_string(),
                    symbol: name,
                    start_line: start + 1,
                    end_line: k + 1,
                    content,
                    kind: ChunkKind::Function,
                });
            }

            i = k + 1;
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"#include <stdio.h>

// A simple helper
int add(int a, int b) {
    return a + b;
}

/**
 * Doc comment for multiply
 */
int multiply(int a, int b) {
    int result = a * b;
    return result;
}

struct Foo {
    int x;
};

namespace bar {
    void inner() {}
}

#define MY_MACRO(x) ((x) + 1)

class KeyCustodian {
public:
    void rotate_keys(int count) {
        for (int i = 0; i < count; i++) {
            do_rotate(i);
        }
    }
};

void deeply_indented() {
    // top-level
    if (true) {
        // nested
    }
}

SOME_MACRO(arg1, arg2) {
    body();
}
"#;

    #[test]
    fn extracts_functions_from_cpp_source() {
        let chunker = CppFunctionChunker::new();
        let chunks = chunker.chunk(SAMPLE, "test.cc");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(names.contains(&"add"), "should find add, got: {names:?}");
        assert!(
            names.contains(&"multiply"),
            "should find multiply, got: {names:?}"
        );
    }

    #[test]
    fn captures_doc_comments_with_functions() {
        let chunker = CppFunctionChunker::new();
        let chunks = chunker.chunk(SAMPLE, "test.cc");
        let mul = chunks.iter().find(|c| c.symbol == "multiply").unwrap();
        assert!(
            mul.content.contains("Doc comment for multiply"),
            "should include doc comment"
        );
    }

    #[test]
    fn extracts_qualified_method_names() {
        let chunker = CppFunctionChunker::new();
        let chunks = chunker.chunk(SAMPLE, "test.cc");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(
            names.contains(&"KeyCustodian::rotate_keys") || names.contains(&"rotate_keys"),
            "should find rotate_keys method, got: {names:?}"
        );
    }

    #[test]
    fn skips_struct_class_namespace_declarations() {
        let chunker = CppFunctionChunker::new();
        let chunks = chunker.chunk(SAMPLE, "test.cc");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(!names.contains(&"Foo"), "should skip struct Foo");
        assert!(!names.contains(&"bar"), "should skip namespace bar");
    }

    #[test]
    fn skips_macro_definitions() {
        let chunker = CppFunctionChunker::new();
        let chunks = chunker.chunk(SAMPLE, "test.cc");
        let names: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(!names.contains(&"MY_MACRO"), "should skip MY_MACRO");
        assert!(!names.contains(&"SOME_MACRO"), "should skip SOME_MACRO");
    }

    #[test]
    fn sets_correct_file_path() {
        let chunker = CppFunctionChunker::new();
        let chunks = chunker.chunk(SAMPLE, "security/keymanager/src/foo.cc");
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].file, "security/keymanager/src/foo.cc");
    }

    #[test]
    fn sets_line_ranges() {
        let chunker = CppFunctionChunker::new();
        let chunks = chunker.chunk(SAMPLE, "test.cc");
        let add = chunks.iter().find(|c| c.symbol == "add").unwrap();
        assert!(add.start_line >= 1);
        assert!(add.end_line > add.start_line);
    }

    #[test]
    fn returns_empty_for_non_code() {
        let chunker = CppFunctionChunker::new();
        let chunks = chunker.chunk(
            "hello world
no code here
",
            "test.txt",
        );
        assert!(chunks.is_empty());
    }

    #[test]
    fn skips_deeply_indented_code() {
        let chunker = CppFunctionChunker::new();
        let src = "        void deeply_nested() {
            return;
        }
";
        let chunks = chunker.chunk(src, "test.cc");
        assert!(chunks.is_empty(), "should skip deeply indented code");
    }

    #[test]
    fn filters_out_tiny_chunks() {
        let chunker = CppFunctionChunker::new();
        let src = "int f() {
}
";
        let chunks = chunker.chunk(src, "test.cc");
        assert!(chunks.is_empty(), "should filter tiny chunks");
    }

    #[test]
    fn all_chunks_have_function_kind() {
        let chunker = CppFunctionChunker::new();
        let chunks = chunker.chunk(SAMPLE, "test.cc");
        for chunk in &chunks {
            assert_eq!(
                chunk.kind,
                ChunkKind::Function,
                "all chunks should be Function kind"
            );
        }
    }
}

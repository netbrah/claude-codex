use codex_vector_search::chunk::Chunker;
use codex_vector_search::cpp_chunker::CppFunctionChunker;

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
    assert!(mul.content.contains("Doc comment for multiply"));
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
    assert!(!names.contains(&"MY_MACRO"));
    assert!(!names.contains(&"SOME_MACRO"));
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
    let add_chunk = chunks.iter().find(|c| c.symbol == "add").unwrap();
    assert!(add_chunk.start_line >= 1);
    assert!(add_chunk.end_line > add_chunk.start_line);
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
    assert!(chunks.is_empty());
}

#[test]
fn filters_out_tiny_chunks() {
    let chunker = CppFunctionChunker::new();
    let src = "int f() {
}
";
    let chunks = chunker.chunk(src, "test.cc");
    assert!(chunks.is_empty());
}

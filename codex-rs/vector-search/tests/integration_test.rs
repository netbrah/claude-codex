//! End-to-end integration tests: chunk → index → search.

use codex_vector_search::AutoChunker;
use codex_vector_search::CppFunctionChunker;
use codex_vector_search::IndexManager;
use codex_vector_search::SearchMode;
use codex_vector_search::SearchParams;
use tempfile::TempDir;

fn make_c_source() -> &'static str {
    r#"
int add(int a, int b) {
    return a + b;
}

int subtract(int a, int b) {
    return a - b;
}

int multiply(int a, int b) {
    int result = a * b;
    return result;
}
"#
}

fn make_python_source() -> &'static str {
    r#"
def add(a, b):
    """Add two numbers."""
    return a + b

def subtract(a, b):
    """Subtract b from a."""
    return a - b

class Calculator:
    def multiply(self, a, b):
        return a * b
"#
}

fn make_json_config() -> &'static str {
    r#"{
  "name": "test-service",
  "version": "2.0.0",
  "config": {
    "port": 8080,
    "host": "localhost"
  },
  "features": {
    "vector_search": true,
    "bm25_fallback": true
  }
}"#
}

fn make_markdown() -> &'static str {
    r#"# Getting Started

This guide covers installation and setup.

## Installation

Run `cargo install myapp` to install.

## Configuration

Edit `config.toml` to configure the application.

## Usage

Run `myapp --help` for usage information.
"#
}

#[tokio::test]
async fn auto_chunker_indexes_mixed_project() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Create a multi-language project in a scope directory
    let scope = "proj";
    let proj = root.join(scope);
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("math.c"), make_c_source()).unwrap();
    std::fs::write(proj.join("calc.py"), make_python_source()).unwrap();
    std::fs::write(proj.join("config.json"), make_json_config()).unwrap();
    std::fs::write(proj.join("README.md"), make_markdown()).unwrap();

    let mut mgr = IndexManager::new(root.to_path_buf(), None, None, Box::new(AutoChunker::new()));

    let results = mgr
        .search(SearchParams {
            query: "add two numbers".to_string(),
            scope: Some(scope.to_string()),
            limit: 10,
            mode: SearchMode::Fulltext,
        })
        .await
        .unwrap();

    assert!(
        !results.is_empty(),
        "should find results across mixed project"
    );

    // Should find the add function from C or Python
    let has_add = results.iter().any(|r| r.symbol.contains("add"));
    assert!(
        has_add,
        "should find 'add' function: {:?}",
        results.iter().map(|r| &r.symbol).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn cpp_chunker_works_standalone() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let scope = "comp";
    let comp = root.join(scope);
    std::fs::create_dir_all(&comp).unwrap();
    std::fs::write(comp.join("math.cc"), make_c_source()).unwrap();

    let mut mgr = IndexManager::new(
        root.to_path_buf(),
        None,
        None,
        Box::new(CppFunctionChunker::new()),
    );

    let results = mgr
        .search(SearchParams {
            query: "multiply numbers".to_string(),
            scope: Some(scope.to_string()),
            limit: 5,
            mode: SearchMode::Fulltext,
        })
        .await
        .unwrap();

    assert!(
        !results.is_empty(),
        "CppFunctionChunker should find results"
    );
    assert!(
        results.iter().any(|r| r.symbol == "multiply"),
        "should find multiply: {:?}",
        results.iter().map(|r| &r.symbol).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn search_with_invalidation() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let scope = "proj";
    let proj = root.join(scope);
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("app.py"), make_python_source()).unwrap();

    let mut mgr = IndexManager::new(root.to_path_buf(), None, None, Box::new(AutoChunker::new()));

    // Initial search
    let results = mgr
        .search(SearchParams {
            query: "add".to_string(),
            scope: Some(scope.to_string()),
            limit: 5,
            mode: SearchMode::Fulltext,
        })
        .await
        .unwrap();
    assert!(!results.is_empty());
    assert_eq!(mgr.status().len(), 1);

    // Invalidate by scope name
    mgr.invalidate_scope(scope);
    assert_eq!(mgr.status().len(), 0);
}

#[tokio::test]
async fn disk_cache_persistence() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cache = tmp.path().join("cache");
    let scope = "src";
    let src_dir = root.join(scope);
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("test.c"), make_c_source()).unwrap();

    // Build index and save to disk
    {
        let mut mgr = IndexManager::new(
            root.to_path_buf(),
            Some(cache.clone()),
            None,
            Box::new(AutoChunker::new()),
        );
        mgr.search(SearchParams {
            query: "add".to_string(),
            scope: Some(scope.to_string()),
            limit: 5,
            mode: SearchMode::Fulltext,
        })
        .await
        .unwrap();
    }

    // New manager should load from disk cache
    {
        let mut mgr = IndexManager::new(
            root.to_path_buf(),
            Some(cache),
            None,
            Box::new(AutoChunker::new()),
        );
        let results = mgr
            .search(SearchParams {
                query: "add".to_string(),
                scope: Some(scope.to_string()),
                limit: 5,
                mode: SearchMode::Fulltext,
            })
            .await
            .unwrap();
        assert!(!results.is_empty(), "should find results from disk cache");
    }
}

#[tokio::test]
async fn python_chunks_contain_class() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let scope = "pyproj";
    let proj = root.join(scope);
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("calc.py"), make_python_source()).unwrap();

    let mut mgr = IndexManager::new(root.to_path_buf(), None, None, Box::new(AutoChunker::new()));

    let results = mgr
        .search(SearchParams {
            query: "Calculator multiply".to_string(),
            scope: Some(scope.to_string()),
            limit: 10,
            mode: SearchMode::Fulltext,
        })
        .await
        .unwrap();

    assert!(
        !results.is_empty(),
        "should find Python class/method results"
    );
}

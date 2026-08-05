//! Tool spec factory for `vector_search` — generic structural similarity search.

use crate::JsonSchema;
use crate::ResponsesApiTool;
use crate::ToolSpec;
use std::collections::BTreeMap;

const DESCRIPTION: &str = "Structural and semantic similarity search across files in a directory. \
Finds similar patterns, related configurations, and structural matches \
using hybrid BM25 keyword + vector embedding search.\n\n\
Works on any file type: code (C, C++, Python, Rust, Go, etc.), JSON configs, \
YAML, Markdown docs, plain text. Chunks files at semantic boundaries \
(functions for code, headings for markdown, keys for JSON) and searches \
by meaning, not just keywords.\n\n\
NOT for exact code analysis -- use grep_search, read_file for precise lookups. \
This tool finds structural similarity, not exact matches. Always verify \
results by reading the actual files.\n\n\
Use cases:\n\
- Find JSON configs with similar structure to this one\n\
- Which markdown docs discuss similar topics?\n\
- Find functions with similar error handling patterns\n\
- What files have similar structure across this directory?\n\n\
First use on a directory indexes all files (~500ms). Subsequent searches are instant.";

pub fn create_vector_search_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "query".to_string(),
            JsonSchema::string(Some(
                "Natural language description of what you're looking for".to_string(),
            )),
        ),
        (
            "path".to_string(),
            JsonSchema::string(Some(
                "Directory to search in (absolute path). Defaults to cwd if omitted".to_string(),
            )),
        ),
        (
            "limit".to_string(),
            JsonSchema::integer(Some("Maximum results (default 10)".to_string())),
        ),
        (
            "mode".to_string(),
            JsonSchema::string_enum(
                vec![
                    serde_json::json!("hybrid"),
                    serde_json::json!("fulltext"),
                    serde_json::json!("vector"),
                ],
                Some(
                    "Search strategy: hybrid (default), fulltext (keyword only), or vector (semantic only)"
                        .to_string(),
                ),
            ),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "vector_search".to_string(),
        description: DESCRIPTION.to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["query".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

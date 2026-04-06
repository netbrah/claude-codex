use super::*;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

#[test]
fn find_files_tool_matches_expected_spec() {
    assert_eq!(
        create_find_files_tool(),
        ToolSpec::Function(ResponsesApiTool {
            name: "find_files".to_string(),
            description:
                "Fast fuzzy file search across directory trees. Finds files and directories by name using a fuzzy matching algorithm, respecting .gitignore rules. Useful for quickly locating files in large repositories without needing external tools like fd or find."
                    .to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::Object {
                properties: BTreeMap::from([
                    (
                        "path".to_string(),
                        JsonSchema::String {
                            description: Some(
                                "Absolute path to the directory to search in. Defaults to the current working directory.".to_string(),
                            ),
                        },
                    ),
                    (
                        "pattern".to_string(),
                        JsonSchema::String {
                            description: Some(
                                "Fuzzy search pattern to match against file and directory paths."
                                    .to_string(),
                            ),
                        },
                    ),
                ]),
                required: Some(vec!["pattern".to_string()]),
                additional_properties: Some(false.into()),
            },
            output_schema: None,
        })
    );
}

#[test]
fn list_dir_tool_matches_expected_spec() {
    assert_eq!(
        create_list_dir_tool(),
        ToolSpec::Function(ResponsesApiTool {
            name: "list_dir".to_string(),
            description:
                "Lists entries in a local directory with 1-indexed entry numbers and simple type labels."
                    .to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::Object {
                properties: BTreeMap::from([
                    (
                        "depth".to_string(),
                        JsonSchema::Number {
                            description: Some(
                                "The maximum directory depth to traverse. Must be 1 or greater."
                                    .to_string(),
                            ),
                        },
                    ),
                    (
                        "dir_path".to_string(),
                        JsonSchema::String {
                            description: Some(
                                "Absolute path to the directory to list.".to_string(),
                            ),
                        },
                    ),
                    (
                        "limit".to_string(),
                        JsonSchema::Number {
                            description: Some(
                                "The maximum number of entries to return.".to_string(),
                            ),
                        },
                    ),
                    (
                        "offset".to_string(),
                        JsonSchema::Number {
                            description: Some(
                                "The entry number to start listing from. Must be 1 or greater."
                                    .to_string(),
                            ),
                        },
                    ),
                ]),
                required: Some(vec!["dir_path".to_string()]),
                additional_properties: Some(false.into()),
            },
            output_schema: None,
        })
    );
}

#[test]
fn test_sync_tool_matches_expected_spec() {
    assert_eq!(
        create_test_sync_tool(),
        ToolSpec::Function(ResponsesApiTool {
            name: "test_sync_tool".to_string(),
            description: "Internal synchronization helper used by Codex integration tests."
                .to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::Object {
                properties: BTreeMap::from([
                    (
                        "barrier".to_string(),
                        JsonSchema::Object {
                            properties: BTreeMap::from([
                                (
                                    "id".to_string(),
                                    JsonSchema::String {
                                        description: Some(
                                            "Identifier shared by concurrent calls that should rendezvous"
                                                .to_string(),
                                        ),
                                    },
                                ),
                                (
                                    "participants".to_string(),
                                    JsonSchema::Number {
                                        description: Some(
                                            "Number of tool calls that must arrive before the barrier opens"
                                                .to_string(),
                                        ),
                                    },
                                ),
                                (
                                    "timeout_ms".to_string(),
                                    JsonSchema::Number {
                                        description: Some(
                                            "Maximum time in milliseconds to wait at the barrier"
                                                .to_string(),
                                        ),
                                    },
                                ),
                            ]),
                            required: Some(vec![
                                "id".to_string(),
                                "participants".to_string(),
                            ]),
                            additional_properties: Some(false.into()),
                        },
                    ),
                    (
                        "sleep_after_ms".to_string(),
                        JsonSchema::Number {
                            description: Some(
                                "Optional delay in milliseconds after completing the barrier"
                                    .to_string(),
                            ),
                        },
                    ),
                    (
                        "sleep_before_ms".to_string(),
                        JsonSchema::Number {
                            description: Some(
                                "Optional delay in milliseconds before any other action"
                                    .to_string(),
                            ),
                        },
                    ),
                ]),
                required: None,
                additional_properties: Some(false.into()),
            },
            output_schema: None,
        })
    );
}

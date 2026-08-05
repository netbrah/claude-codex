use super::*;
use crate::JsonSchema;
use crate::ResponsesApiTool;
use crate::ToolSpec;
use codex_api::OPENAI_MAX_TOOL_NAME_LENGTH;

#[test]
fn truncate_long_tool_name_on_responses_wire() {
    let long_name = "m".repeat(80);
    let tools = vec![ToolSpec::Function(ResponsesApiTool {
        name: long_name.clone(),
        description: "d".into(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(Default::default(), None, None),
        output_schema: None,
    })];
    let (json, mapping) = prepare_responses_wire_tools(&tools).expect("prepare");
    let name = json[0]["name"].as_str().unwrap();
    assert_eq!(name.len(), OPENAI_MAX_TOOL_NAME_LENGTH);
    assert_eq!(mapping.get(name).map(String::as_str), Some(long_name.as_str()));
}

#[test]
fn strict_schema_sanitizes_object_properties() {
    let tools = vec![ToolSpec::Function(ResponsesApiTool {
        name: "shell".into(),
        description: "run".into(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            [(
                "path".to_string(),
                JsonSchema::string(None),
            )]
            .into_iter()
            .collect(),
            None,
            None,
        ),
        output_schema: None,
    })];
    let (json, _) = prepare_responses_wire_tools(&tools).expect("prepare");
    let params = &json[0]["parameters"];
    assert_eq!(params["additionalProperties"], serde_json::json!(false));
    assert!(params["required"].as_array().unwrap().contains(&serde_json::json!("path")));
    assert_eq!(json[0]["strict"], serde_json::json!(true));
}

#[test]
fn gemini_wire_tool_names_include_namespace_prefix() {
    use crate::ResponsesApiNamespace;
    let tools = vec![ToolSpec::Namespace(ResponsesApiNamespace {
        name: "mcp__srv__".into(),
        description: "ns".into(),
        tools: vec![ResponsesApiNamespaceTool::Function(ResponsesApiTool {
            name: "read_file".into(),
            description: "read".into(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(Default::default(), None, None),
            output_schema: None,
        })],
    })];
    assert_eq!(
        gemini_wire_tool_names(&tools),
        vec!["mcp__srv__read_file".to_string()]
    );
}

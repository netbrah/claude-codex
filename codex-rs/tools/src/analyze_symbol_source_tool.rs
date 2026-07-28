use crate::JsonSchema;
use crate::ResponsesApiTool;
use crate::ToolSpec;
use std::collections::BTreeMap;

pub fn create_analyze_symbol_source_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "symbol".to_string(),
            JsonSchema::string(Some(
                "Symbol name to analyze (plain or qualified, e.g. `my_func` or 
                 `MyStruct::my_method`)."
                    .to_string(),
            )),
        ),
        (
            "scopePath".to_string(),
            JsonSchema::string(Some(
                "Directory to search in. Defaults to the session's working directory.".to_string(),
            )),
        ),
        (
            "maxCallers".to_string(),
            JsonSchema::number(Some(
                "Maximum number of callers (references) to return (default 15).".to_string(),
            )),
        ),
        (
            "maxCallees".to_string(),
            JsonSchema::number(Some(
                "Maximum number of callees to extract from the definition (default 20)."
                    .to_string(),
            )),
        ),
        (
            "contextLines".to_string(),
            JsonSchema::number(Some(
                "Number of source lines to include around the definition (default 50).".to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "analyze_symbol_source".to_string(),
        description: "Analyzes a symbol in the local workspace using ripgrep-backed search. 
                       Returns the definition location, callers (references), and callees extracted 
                       from the definition source."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["symbol".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

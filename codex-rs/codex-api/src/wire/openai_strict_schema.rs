//! Recursive JSON Schema sanitizer for OpenAI Responses strict tool mode.
//!
//! Ported from LiteLLM `_add_additional_properties_false`:
//! `upstream-infrastructure/litellm/.../adapters/transformation.py:875-912`.

use serde_json::Value;

/// Ensure object schemas comply with OpenAI strict mode:
/// `additionalProperties: false` and all property keys in `required`.
pub fn ensure_openai_strict(schema: &mut Value) {
    match schema {
        Value::Object(obj) => {
            if obj.get("type").and_then(Value::as_str) == Some("object")
                && let Some(props) = obj.get("properties").and_then(Value::as_object)
            {
                let keys: Vec<Value> = props.keys().map(|k| Value::String(k.clone())).collect();
                obj.insert("additionalProperties".to_string(), Value::Bool(false));
                obj.insert("required".to_string(), Value::Array(keys));
            }
            if let Some(props) = obj
                .get_mut("properties")
                .and_then(Value::as_object_mut)
            {
                for prop in props.values_mut() {
                    ensure_openai_strict(prop);
                }
            }

            if let Some(items) = obj.get_mut("items") {
                ensure_openai_strict(items);
            }

            for key in ["anyOf", "oneOf", "allOf"] {
                if let Some(Value::Array(variants)) = obj.get_mut(key) {
                    for variant in variants {
                        ensure_openai_strict(variant);
                    }
                }
            }

            for key in ["$defs", "definitions"] {
                if let Some(Value::Object(defs)) = obj.get_mut(key) {
                    for def_schema in defs.values_mut() {
                        ensure_openai_strict(def_schema);
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                ensure_openai_strict(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn object_schema_gets_additional_properties_false_and_required() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "limit": { "type": "integer" }
            }
        });
        ensure_openai_strict(&mut schema);
        assert_eq!(schema["additionalProperties"], json!(false));
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 2);
        assert!(required.contains(&json!("path")));
        assert!(required.contains(&json!("limit")));
    }

    #[test]
    fn nested_anyof_and_defs_recursed() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "value": {
                    "anyOf": [
                        { "type": "object", "properties": { "a": { "type": "string" } } }
                    ]
                }
            },
            "$defs": {
                "Inner": { "type": "object", "properties": { "b": { "type": "number" } } }
            }
        });
        ensure_openai_strict(&mut schema);
        assert_eq!(
            schema["properties"]["value"]["anyOf"][0]["additionalProperties"],
            json!(false)
        );
        assert_eq!(schema["$defs"]["Inner"]["additionalProperties"], json!(false));
    }

    #[test]
    fn non_object_schema_unchanged() {
        let mut schema = json!({ "type": "string" });
        let before = schema.clone();
        ensure_openai_strict(&mut schema);
        assert_eq!(schema, before);
    }
}

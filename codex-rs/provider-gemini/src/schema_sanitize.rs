//! Sanitize JSON Schema for Gemini's `functionDeclarations.parameters`.
//!
//! Walks the schema and applies eight transforms that align JSON Schema
//! (as XLI and MCP servers emit it) to Gemini's accepted subset:
//!
//! 1. Inline `$ref` against `$defs` / `definitions` (recursive, cycle-safe).
//! 2. Rewrite `type: ["T", "null"]` → `type: "T", nullable: true`.
//! 3. Strip `pattern` keys at any depth.
//! 4. Filter `format` to Gemini's whitelist; drop unknown values.
//! 5. Demote `oneOf` (depth ≤ 1) → `anyOf`; drop `allOf`; cap `anyOf` depth at 2.
//! 6. Strip metadata: `$schema`, `$id`, `$comment`, `title`, `examples`,
//!    `readOnly`, `writeOnly`, `deprecated`, `default`.
//! 7. Empty-object guard: `{}` → `{"type": "object", "properties": {}}`.
//! 8. Recursion depth cap at 16; beyond that emit `{"type": "object"}` and warn.
//!
//! Idempotent: `sanitize_for_gemini(sanitize_for_gemini(s)) == sanitize_for_gemini(s)`.

use serde_json::Value;
use std::collections::HashSet;
use tracing::warn;

/// Maximum recursion depth before replacing with `{type: "object"}`.
const MAX_DEPTH: usize = 16;

/// Metadata keys Gemini ignores or rejects.
const METADATA_KEYS: &[&str] = &[
    "$schema",
    "$id",
    "$comment",
    "title",
    "examples",
    "readOnly",
    "writeOnly",
    "deprecated",
    "default",
];

/// Gemini-supported `format` values.
const SUPPORTED_FORMATS: &[&str] = &[
    "date-time",
    "date",
    "time",
    "duration",
    "email",
    "hostname",
    "ipv4",
    "ipv6",
    "uri",
    "uuid",
    "enum",
    "int32",
    "int64",
    "float",
    "double",
    "byte",
];

/// Sanitize a JSON Schema for Gemini's `functionDeclarations.parameters`.
///
/// Takes `&Value`, returns an owned sanitized copy. Never mutates the input.
pub fn sanitize_for_gemini(schema: &Value) -> Value {
    let mut out = schema.clone();

    // Transform 1: resolve $ref/$defs.
    if let Some(defs) = extract_defs(&out) {
        let chain = HashSet::new();
        resolve_refs(&mut out, &defs, &chain);
    }
    // Remove $defs/definitions from root.
    if let Value::Object(map) = &mut out {
        map.remove("$defs");
        map.remove("defs");
        map.remove("definitions");
    }

    // Transforms 2-8: recursive.
    sanitize_node(&mut out, 0);

    // Transform 7: empty-object guard at root.
    if let Value::Object(map) = &mut out
        && (map.is_empty() || !map.contains_key("type"))
    {
        map.insert("type".to_string(), Value::String("object".to_string()));
        if !map.contains_key("properties") {
            map.insert(
                "properties".to_string(),
                Value::Object(serde_json::Map::new()),
            );
        }
    }

    out
}

// ---- Transform 1: $ref resolution ----

fn extract_defs(schema: &Value) -> Option<serde_json::Map<String, Value>> {
    let obj = schema.as_object()?;
    let defs_val = obj
        .get("$defs")
        .or_else(|| obj.get("defs"))
        .or_else(|| obj.get("definitions"))?;
    defs_val.as_object().cloned()
}

fn resolve_refs(value: &mut Value, defs: &serde_json::Map<String, Value>, chain: &HashSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(ref_val) = map.get("$ref").cloned()
                && let Some(ref_str) = ref_val.as_str()
            {
                let ref_name = ref_str.to_string();
                if chain.contains(&ref_name) {
                    warn!(
                        ref_path = %ref_str,
                        "Gemini schema sanitizer detected cyclic $ref; replacing with {{type: object}}"
                    );
                    *value = serde_json::json!({"type": "object"});
                    return;
                }
                if let Some(resolved) = resolve_ref_path(ref_str, defs) {
                    let mut resolved = resolved.clone();
                    let mut deeper = chain.clone();
                    deeper.insert(ref_name);
                    resolve_refs(&mut resolved, defs, &deeper);
                    *value = resolved;
                    return;
                }
            }
            for v in map.values_mut() {
                resolve_refs(v, defs, chain);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                resolve_refs(v, defs, chain);
            }
        }
        _ => {}
    }
}

fn resolve_ref_path<'a>(
    ref_path: &str,
    defs: &'a serde_json::Map<String, Value>,
) -> Option<&'a Value> {
    let name = ref_path
        .strip_prefix("#/$defs/")
        .or_else(|| ref_path.strip_prefix("#/defs/"))
        .or_else(|| ref_path.strip_prefix("#/definitions/"))?;
    defs.get(name)
}

// ---- Transforms 2-8: recursive node cleanup ----

fn sanitize_node(value: &mut Value, depth: usize) {
    // Transform 8: depth cap.
    if depth > MAX_DEPTH {
        warn!(
            depth,
            "Gemini schema sanitizer hit depth cap; replacing with {{type: object}}"
        );
        *value = serde_json::json!({"type": "object"});
        return;
    }

    let Value::Object(map) = value else { return };

    // Transform 6: strip metadata keys.
    for key in METADATA_KEYS {
        map.remove(*key);
    }

    // Strip leftover $ref/$defs.
    map.remove("$ref");
    map.remove("$defs");
    map.remove("defs");
    map.remove("definitions");

    // Transform 2: rewrite type: ["T", "null"] → {type: "T", nullable: true}.
    rewrite_nullable_type(map);

    // Transform 3: strip `pattern`.
    map.remove("pattern");

    // Transform 4: filter unsupported `format` values.
    if let Some(fmt) = map.get("format").and_then(Value::as_str)
        && !SUPPORTED_FORMATS.contains(&fmt)
    {
        map.remove("format");
    }

    // Transform 5: allOf → merge, oneOf → anyOf.
    normalize_combinators(map, depth);

    // Recurse into sub-schemas.
    for key in &["properties", "patternProperties"] {
        if let Some(props) = map.get_mut(*key)
            && let Value::Object(props_map) = props
        {
            for v in props_map.values_mut() {
                sanitize_node(v, depth + 1);
            }
        }
    }
    if let Some(items) = map.get_mut("items") {
        sanitize_node(items, depth + 1);
    }
    if let Some(any_of) = map.get_mut("anyOf")
        && let Value::Array(arr) = any_of
    {
        for v in arr.iter_mut() {
            sanitize_node(v, depth + 1);
        }
    }
    if let Some(additional) = map.get_mut("additionalProperties")
        && additional.is_object()
    {
        sanitize_node(additional, depth + 1);
    }
}

// ---- Transform 2 ----

fn rewrite_nullable_type(map: &mut serde_json::Map<String, Value>) {
    let Some(type_val) = map.get("type") else {
        return;
    };
    let Value::Array(types) = type_val else {
        return;
    };

    let type_strings: Vec<String> = types
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    let has_null = type_strings.iter().any(|t| t == "null");
    let non_null: Vec<&str> = type_strings
        .iter()
        .filter(|t| t.as_str() != "null")
        .map(String::as_str)
        .collect();

    if has_null {
        match non_null.len() {
            0 => {
                map.insert("type".to_string(), Value::String("string".to_string()));
                map.insert("nullable".to_string(), Value::Bool(true));
            }
            1 => {
                map.insert("type".to_string(), Value::String(non_null[0].to_string()));
                map.insert("nullable".to_string(), Value::Bool(true));
            }
            _ => {
                warn!(
                    types = ?non_null,
                    "Gemini schema sanitizer: multi-type union with null; keeping first type"
                );
                map.insert("type".to_string(), Value::String(non_null[0].to_string()));
                map.insert("nullable".to_string(), Value::Bool(true));
            }
        }
    } else if non_null.len() > 1 {
        // Type array without null — Gemini doesn't support type arrays.
        // Keep the first type and warn.
        warn!(
            types = ?non_null,
            "Gemini schema sanitizer: type array without null; keeping first type"
        );
        map.insert("type".to_string(), Value::String(non_null[0].to_string()));
    }
    // Single-element array without null: just unwrap to scalar.
    else if non_null.len() == 1 {
        map.insert("type".to_string(), Value::String(non_null[0].to_string()));
    }
}

// ---- Transform 5 ----

fn normalize_combinators(map: &mut serde_json::Map<String, Value>, depth: usize) {
    // allOf → shallow merge into parent. Properties maps are deep-merged
    // so that allOf patches from multiple items accumulate correctly.
    if let Some(Value::Array(all_of)) = map.remove("allOf") {
        for item in all_of {
            if let Value::Object(sub) = item {
                for (k, v) in sub {
                    if k == "properties" {
                        // Deep-merge properties: combine sub-properties.
                        if let Value::Object(new_props) = v {
                            let existing = map
                                .entry("properties")
                                .or_insert_with(|| Value::Object(serde_json::Map::new()));
                            if let Value::Object(existing_props) = existing {
                                for (pk, pv) in new_props {
                                    existing_props.entry(pk).or_insert(pv);
                                }
                            }
                        }
                    } else if k == "required" {
                        // Merge required arrays.
                        if let Value::Array(new_req) = v {
                            let existing = map
                                .entry("required")
                                .or_insert_with(|| Value::Array(Vec::new()));
                            if let Value::Array(existing_req) = existing {
                                for r in new_req {
                                    if !existing_req.contains(&r) {
                                        existing_req.push(r);
                                    }
                                }
                            }
                        }
                    } else {
                        map.entry(k).or_insert(v);
                    }
                }
            } else {
                warn!("Gemini schema sanitizer: allOf item is not an object; dropped");
            }
        }
    }

    // oneOf → anyOf at depth ≤ 1; deeper → drop with warn.
    if let Some(one_of) = map.remove("oneOf")
        && let Value::Array(ref arr) = one_of
    {
        if depth <= 1 {
            map.insert("anyOf".to_string(), one_of);
        } else {
            warn!(
                depth,
                variants = arr.len(),
                "Gemini schema sanitizer dropped deep oneOf"
            );
        }
    }

    // Cap anyOf depth at 2; deeper → drop with warn.
    if depth > 2
        && let Some(Value::Array(ref arr)) = map.remove("anyOf")
    {
        warn!(
            depth,
            variants = arr.len(),
            "Gemini schema sanitizer dropped deep anyOf"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- Transform 1: $ref resolution ----

    #[test]
    fn t1_inline_ref_simple() {
        let schema = json!({
            "type": "object",
            "$defs": {
                "Color": { "type": "string", "enum": ["red", "green", "blue"] }
            },
            "properties": {
                "color": { "$ref": "#/$defs/Color" }
            }
        });
        let out = sanitize_for_gemini(&schema);
        assert!(out.get("$defs").is_none());
        assert_eq!(
            out["properties"]["color"],
            json!({"type": "string", "enum": ["red", "green", "blue"]})
        );
    }

    #[test]
    fn t1_inline_ref_recursive() {
        let schema = json!({
            "type": "object",
            "$defs": {
                "Inner": { "type": "integer" },
                "Outer": { "$ref": "#/$defs/Inner" }
            },
            "properties": {
                "value": { "$ref": "#/$defs/Outer" }
            }
        });
        let out = sanitize_for_gemini(&schema);
        assert_eq!(out["properties"]["value"], json!({"type": "integer"}));
    }

    #[test]
    fn t1_inline_ref_cyclic_replaced_with_object() {
        let schema = json!({
            "type": "object",
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "child": { "$ref": "#/$defs/Node" }
                    }
                }
            },
            "properties": {
                "root": { "$ref": "#/$defs/Node" }
            }
        });
        let out = sanitize_for_gemini(&schema);
        // The cyclic ref should become {type: "object"}.
        assert_eq!(
            out["properties"]["root"]["properties"]["child"],
            json!({"type": "object"})
        );
    }

    #[test]
    fn t1_definitions_key_supported() {
        let schema = json!({
            "type": "object",
            "definitions": {
                "Id": { "type": "string" }
            },
            "properties": {
                "id": { "$ref": "#/definitions/Id" }
            }
        });
        let out = sanitize_for_gemini(&schema);
        assert!(out.get("definitions").is_none());
        assert_eq!(out["properties"]["id"], json!({"type": "string"}));
    }

    // ---- Transform 2: nullable type ----

    #[test]
    fn t2_type_array_with_null_becomes_nullable() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": ["string", "null"] },
                "age": { "type": ["null", "integer"] }
            }
        });
        let out = sanitize_for_gemini(&schema);
        assert_eq!(
            out["properties"]["name"],
            json!({"type": "string", "nullable": true})
        );
        assert_eq!(
            out["properties"]["age"],
            json!({"type": "integer", "nullable": true})
        );
    }

    #[test]
    fn t2_type_array_without_null_keeps_first_type_with_warn() {
        // Non-null multi-type: ["string", "integer"] → keep "string" + warn.
        let schema = json!({
            "type": "object",
            "properties": {
                "mixed": { "type": ["string", "integer"] }
            }
        });
        let out = sanitize_for_gemini(&schema);
        // Should keep "string" (first type) — no nullable since no null.
        assert_eq!(out["properties"]["mixed"]["type"], "string");
    }

    // ---- Transform 3: pattern ----

    #[test]
    fn t3_pattern_stripped_at_all_depths() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "pattern": "^/.*" },
                "nested": {
                    "type": "object",
                    "properties": {
                        "inner": { "type": "string", "pattern": "^[a-z]+" }
                    }
                }
            }
        });
        let out = sanitize_for_gemini(&schema);
        assert!(out["properties"]["path"].get("pattern").is_none());
        assert!(
            out["properties"]["nested"]["properties"]["inner"]
                .get("pattern")
                .is_none()
        );
    }

    // ---- Transform 4: format ----

    #[test]
    fn t4_unknown_format_dropped() {
        let schema = json!({
            "type": "object",
            "properties": {
                "card": { "type": "string", "format": "credit-card" },
                "ssn": { "type": "string", "format": "social-security" }
            }
        });
        let out = sanitize_for_gemini(&schema);
        assert!(out["properties"]["card"].get("format").is_none());
        assert!(out["properties"]["ssn"].get("format").is_none());
    }

    #[test]
    fn t4_known_format_preserved() {
        let schema = json!({
            "type": "object",
            "properties": {
                "ts": { "type": "string", "format": "date-time" },
                "f": { "type": "number", "format": "float" },
                "u": { "type": "string", "format": "uri" },
                "id": { "type": "string", "format": "uuid" },
                "e": { "type": "string", "format": "email" },
                "n": { "type": "integer", "format": "int64" }
            }
        });
        let out = sanitize_for_gemini(&schema);
        assert_eq!(out["properties"]["ts"]["format"], "date-time");
        assert_eq!(out["properties"]["f"]["format"], "float");
        assert_eq!(out["properties"]["u"]["format"], "uri");
        assert_eq!(out["properties"]["id"]["format"], "uuid");
        assert_eq!(out["properties"]["e"]["format"], "email");
        assert_eq!(out["properties"]["n"]["format"], "int64");
    }

    // ---- Transform 5: combinators ----

    #[test]
    fn t5_one_of_becomes_any_of() {
        let schema = json!({
            "type": "object",
            "properties": {
                "val": {
                    "oneOf": [
                        { "type": "string" },
                        { "type": "integer" }
                    ]
                }
            }
        });
        let out = sanitize_for_gemini(&schema);
        assert!(out["properties"]["val"].get("oneOf").is_none());
        assert_eq!(
            out["properties"]["val"]["anyOf"],
            json!([{"type": "string"}, {"type": "integer"}])
        );
    }

    #[test]
    fn t5_all_of_merged() {
        let schema = json!({
            "allOf": [
                { "type": "object", "properties": { "a": { "type": "string" } } },
                { "properties": { "b": { "type": "integer" } } }
            ]
        });
        let out = sanitize_for_gemini(&schema);
        assert!(out.get("allOf").is_none());
        assert_eq!(out["properties"]["a"]["type"], "string");
        assert_eq!(out["properties"]["b"]["type"], "integer");
    }

    #[test]
    fn t5_deep_oneof_dropped() {
        let schema = json!({
            "type": "object",
            "properties": {
                "l1": {
                    "type": "object",
                    "properties": {
                        "l2": {
                            "oneOf": [
                                { "type": "string" },
                                { "type": "integer" }
                            ]
                        }
                    }
                }
            }
        });
        let out = sanitize_for_gemini(&schema);
        // oneOf at depth 2 (root=0, l1=1, l2=2) exceeds limit of 1 → dropped.
        let l2 = &out["properties"]["l1"]["properties"]["l2"];
        assert!(l2.get("oneOf").is_none());
        assert!(l2.get("anyOf").is_none());
    }

    // ---- Transform 6: metadata ----

    #[test]
    fn t6_metadata_keys_removed() {
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "$id": "test",
            "$comment": "internal",
            "title": "MyTool",
            "examples": [{"a": 1}],
            "readOnly": true,
            "writeOnly": false,
            "deprecated": true,
            "default": {},
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "title": "Name Field",
                    "examples": ["foo"],
                    "default": "bar"
                }
            }
        });
        let out = sanitize_for_gemini(&schema);
        for key in METADATA_KEYS {
            assert!(out.get(*key).is_none(), "root {key} should be stripped");
            assert!(
                out["properties"]["name"].get(*key).is_none(),
                "nested {key} should be stripped"
            );
        }
        assert_eq!(out["type"], "object");
    }

    // ---- Transform 7: empty-object guard ----

    #[test]
    fn t7_empty_object_guard() {
        let schema = json!({});
        let out = sanitize_for_gemini(&schema);
        assert_eq!(out["type"], "object");
        assert!(out.get("properties").is_some());
    }

    #[test]
    fn t7_schema_without_type_gets_object() {
        let schema = json!({"description": "no type"});
        let out = sanitize_for_gemini(&schema);
        assert_eq!(out["type"], "object");
    }

    // ---- Transform 8: depth cap ----

    #[test]
    fn t8_depth_cap_enforced() {
        let mut inner = json!({"type": "string"});
        for _ in 0..20 {
            inner = json!({
                "type": "object",
                "properties": { "nested": inner }
            });
        }
        let out = sanitize_for_gemini(&inner);
        // Walk to depth cap — should be {type: "object"}.
        let mut node = &out;
        for _ in 0..MAX_DEPTH {
            if let Some(nested) = node.get("properties").and_then(|p| p.get("nested")) {
                node = nested;
            } else {
                break;
            }
        }
        assert_eq!(node["type"], "object");
    }

    // ---- Idempotent ----

    #[test]
    fn idempotent() {
        let schema = json!({
            "type": "object",
            "$defs": {
                "Ref": { "type": "string", "pattern": "^/.*", "format": "credit-card" }
            },
            "$schema": "http://json-schema.org/draft-07",
            "title": "Test",
            "properties": {
                "a": { "$ref": "#/$defs/Ref" },
                "b": { "type": ["string", "null"] },
                "c": { "oneOf": [{"type": "string"}, {"type": "integer"}] }
            }
        });
        let first = sanitize_for_gemini(&schema);
        let second = sanitize_for_gemini(&first);
        assert_eq!(first, second);
    }

    // ---- Edge cases ----

    #[test]
    fn no_defs_no_crash() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            }
        });
        let out = sanitize_for_gemini(&schema);
        assert_eq!(out["properties"]["name"]["type"], "string");
    }

    #[test]
    fn shell_tool_schema_passthrough() {
        let schema = json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "The command and arguments"
                }
            },
            "required": ["command"],
            "additionalProperties": false
        });
        let out = sanitize_for_gemini(&schema);
        assert_eq!(out, schema, "simple schema should pass through unchanged");
    }

    #[test]
    fn complex_mcp_schema() {
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "MCP Tool",
            "type": "object",
            "$defs": {
                "FileRef": {
                    "type": "string",
                    "format": "credit-card",
                    "pattern": "^file://.*"
                }
            },
            "properties": {
                "target": { "$ref": "#/$defs/FileRef" },
                "label": { "type": ["string", "null"] },
                "count": { "type": "integer", "format": "int32" }
            },
            "required": ["target"],
            "examples": [{"target": "file://foo"}]
        });
        let out = sanitize_for_gemini(&schema);
        assert!(out.get("$defs").is_none());
        assert!(out.get("$schema").is_none());
        assert!(out.get("title").is_none());
        assert!(out.get("examples").is_none());
        assert_eq!(out["properties"]["target"]["type"], "string");
        assert!(out["properties"]["target"].get("format").is_none());
        assert!(out["properties"]["target"].get("pattern").is_none());
        assert_eq!(out["properties"]["label"]["type"], "string");
        assert_eq!(out["properties"]["label"]["nullable"], true);
        assert_eq!(out["properties"]["count"]["format"], "int32");
    }

    /// Passthrough invariant: schema features that Gemini accepts
    /// natively MUST survive the sanitizer untouched.
    ///
    /// This is the inverse of the existing "what we strip" tests. We
    /// already pin every transform that drops or rewrites a key. This
    /// test pins the OPPOSITE — every key that should be left alone
    /// is left alone — so a future "be more aggressive" change that
    /// over-strips legitimate schema features fails the test instead
    /// of silently degrading every Gemini tool call.
    ///
    /// Add new entries here whenever the upstream JSON Schema dialect
    /// XLI tools emit gains a new feature that Gemini accepts.
    #[test]
    fn passthrough_features_gemini_accepts_natively() {
        let schema = json!({
            "type": "object",
            "description": "Top-level description must survive.",
            "properties": {
                "name":        { "type": "string", "description": "Field-level desc kept." },
                "count":       { "type": "integer", "minimum": 0, "maximum": 100 },
                "rate":        { "type": "number", "exclusiveMinimum": 0.0 },
                "options":     { "type": "string", "enum": ["a", "b", "c"] },
                "tags":        { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 10 },
                "nested":      {
                    "type": "object",
                    "properties": {
                        "x": { "type": "string" }
                    },
                    "required": ["x"]
                },
                "any_of_field": { "anyOf": [{"type":"string"}, {"type":"integer"}] },
                "format_kept":  { "type": "string", "format": "date-time" },
                "additional":   { "type": "object", "additionalProperties": false }
            },
            "required": ["name"]
        });

        let out = sanitize_for_gemini(&schema);

        // Top-level survives.
        assert_eq!(out["type"], "object");
        assert_eq!(out["description"], "Top-level description must survive.");
        assert_eq!(out["required"][0], "name");

        let p = &out["properties"];

        // Field descriptions survive (Gemini USES these — stripping them
        // degrades tool-call quality).
        assert_eq!(p["name"]["description"], "Field-level desc kept.");

        // Numeric constraints survive.
        assert_eq!(p["count"]["type"], "integer");
        assert_eq!(p["count"]["minimum"], 0);
        assert_eq!(p["count"]["maximum"], 100);
        assert_eq!(p["rate"]["exclusiveMinimum"], 0.0);

        // enum survives — Gemini USES it for constrained outputs.
        assert_eq!(p["options"]["enum"][0], "a");
        assert_eq!(p["options"]["enum"][2], "c");

        // Array constraints survive.
        assert_eq!(p["tags"]["items"]["type"], "string");
        assert_eq!(p["tags"]["minItems"], 1);
        assert_eq!(p["tags"]["maxItems"], 10);

        // Nested object structure preserved (recursion correctness).
        assert_eq!(p["nested"]["properties"]["x"]["type"], "string");
        assert_eq!(p["nested"]["required"][0], "x");

        // anyOf survives at top depth (only deep oneOf is dropped).
        assert!(p["any_of_field"]["anyOf"].is_array());

        // Whitelisted format survives.
        assert_eq!(p["format_kept"]["format"], "date-time");

        // additionalProperties: false survives — common in MCP tool schemas
        // for strict tool-arg validation.
        assert_eq!(p["additional"]["additionalProperties"], false);
    }
}

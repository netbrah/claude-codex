//! JSON-RPC 2.0 message types + the subset of Copilot-CLI method shapes we
//! care about for the host side.
//!
//! We keep the message enum intentionally loose — `params` and `result` stay
//! as `serde_json::Value` so that supporting new methods is a routing-table
//! change rather than a codec change. Typed helpers live alongside for the
//! methods the host actively participates in.

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

/// A JSON-RPC 2.0 envelope on the wire. The SDK mixes all four message kinds
/// on a single stream, so the host must be able to peek the shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: JsonRpcVersion,
    pub id: Value,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: JsonRpcVersion,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: JsonRpcVersion,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// `"2.0"` literal, serde-enforced.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum JsonRpcVersion {
    #[default]
    #[serde(rename = "2.0")]
    V2_0,
}

/// Standard JSON-RPC + LSP error codes we route.
pub mod error_codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

// -----------------------------------------------------------------------------
// Copilot-CLI typed shapes (minimum needed for MVP host)
// -----------------------------------------------------------------------------

/// Parameters for `session.resume`, issued by the child when the SDK
/// `joinSession()` resolves.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumeParams {
    pub session_id: String,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<ToolRegistration>>,
    #[serde(default)]
    pub commands: Option<Vec<CommandRegistration>>,
    /// Remaining fields from `session.resume` are preserved verbatim so we
    /// can inspect / log them without schema drift breakage.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRegistration {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<Value>,
    #[serde(default)]
    pub overrides_built_in_tool: Option<bool>,
    #[serde(default)]
    pub skip_permission: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRegistration {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Response body for `session.resume`.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumeResult {
    pub workspace_path: String,
    pub capabilities: SessionCapabilities,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    /// Schema version the host claims. `1` is the current public SDK level.
    pub protocol_version: u32,
    pub negotiated_protocol_version: u32,
    pub streaming: bool,
    pub vision: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{"message":"hi"}}"#;
        let m: JsonRpcMessage = serde_json::from_str(raw).unwrap();
        match m {
            JsonRpcMessage::Request(r) => {
                assert_eq!(r.method, "ping");
                assert_eq!(r.id, serde_json::json!(1));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parses_notification() {
        let raw = r#"{"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"s"}}"#;
        let m: JsonRpcMessage = serde_json::from_str(raw).unwrap();
        assert!(matches!(m, JsonRpcMessage::Notification(_)));
    }

    #[test]
    fn parses_response_ok() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
        let m: JsonRpcMessage = serde_json::from_str(raw).unwrap();
        assert!(matches!(m, JsonRpcMessage::Response(_)));
    }

    #[test]
    fn parses_response_err() {
        let raw =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#;
        let m: JsonRpcMessage = serde_json::from_str(raw).unwrap();
        let JsonRpcMessage::Response(r) = m else {
            panic!("wrong");
        };
        assert!(r.error.is_some());
        assert_eq!(r.error.unwrap().code, -32601);
    }

    #[test]
    fn session_resume_params_tolerate_unknown_fields() {
        let raw = r#"{
            "sessionId": "abc",
            "clientName": "xli",
            "tools": [{"name":"t","description":"d","parameters":{"type":"object"}}],
            "requestPermission": true,
            "envValueMode": "direct"
        }"#;
        let p: SessionResumeParams = serde_json::from_str(raw).unwrap();
        assert_eq!(p.session_id, "abc");
        assert_eq!(p.tools.as_ref().unwrap().len(), 1);
        assert!(p.extra.contains_key("requestPermission"));
    }
}

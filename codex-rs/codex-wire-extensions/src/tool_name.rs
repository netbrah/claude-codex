use serde::Deserialize;
use serde::Serialize;
use std::cmp::Ordering;
use std::fmt;

/// Identifies a callable tool, preserving the namespace split when the model
/// provides one.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ToolName {
    pub name: String,
    pub namespace: Option<String>,
}

impl ToolName {
    pub fn new(namespace: Option<String>, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            namespace,
        }
    }

    pub fn plain(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            namespace: None,
        }
    }

    pub fn namespaced(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            namespace: Some(namespace.into()),
        }
    }

    /// The single blessed path from a structured [`ToolName`] to the flat
    /// string a flat wire advertises and re-encodes (`<namespace>__<name>`).
    ///
    /// Use this — never [`ToolName`]'s `Display` — to build an outbound wire
    /// name. `Display` is delimiter-less and lossy (see its doc); routing a
    /// wire name through it reintroduces the flat-name regression (R1).
    #[must_use]
    pub fn to_wire_name(&self) -> WireToolName {
        flat_mcp_tool_name(&self.name, self.namespace.as_deref())
    }
}

/// XLI flat-wire delimiter between an MCP namespace and a tool name.
///
/// Anthropic `/messages`, Gemini `/generateContent`, and Copilot
/// chat-completions have no structured `{namespace, name}` split, so XLI
/// flattens namespaced tools to `<namespace>__<name>` on the wire. This is
/// the inverse of the decoder's `parse_flat_mcp_tool_name`.
pub const FLAT_MCP_TOOL_NAME_DELIMITER: &str = "__";

/// Encode a `(name, namespace)` pair into the single flat string that the
/// flat wires advertise in their tool list, so model-visible tool names and
/// re-encoded call history stay byte-identical.
///
/// - namespaced  -> `"<namespace>__<name>"`
/// - unnamespaced -> `"<name>"`
///
/// IMPORTANT: history re-encoders (assistant `tool_use` / `functionCall`
/// blocks) MUST route through this, not the bare `name`. Emitting the bare
/// name desyncs the model's view of its own past calls from the advertised
/// tool list and triggers intermittent "unsupported call" misses.
#[must_use]
pub fn flat_mcp_tool_name(name: &str, namespace: Option<&str>) -> WireToolName {
    let encoded = match namespace {
        Some(namespace) if !namespace.is_empty() => {
            format!("{namespace}{FLAT_MCP_TOOL_NAME_DELIMITER}{name}")
        }
        _ => name.to_string(),
    };
    WireToolName(encoded)
}

/// A tool name encoded for a *flat* wire — Anthropic `/messages`, Gemini
/// `/generateContent`, Copilot chat-completions — which have no structured
/// `{namespace, name}` split.
///
/// The wrapped string is always the delimiter-correct `<namespace>__<name>`
/// (or the bare `<name>` when unnamespaced) because the ONLY constructor is
/// [`flat_mcp_tool_name`] / [`ToolName::to_wire_name`]. There is deliberately
/// no `From<ToolName>` / `From<String>` / `Display`-based constructor: that is
/// what makes [`ToolName`]'s lossy, delimiter-less `Display` impl
/// un-substitutable at any site that has been typed to accept a
/// `WireToolName`. A bare `tool_name.to_string()` will not type-check there,
/// which is the compile-time guard against the flat-name regression (R1/R2).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WireToolName(String);

impl WireToolName {
    /// Borrow the encoded wire name for serialization into a wire payload.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume into the owned encoded wire string.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for WireToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq<&str> for WireToolName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// Human/error/registry-fallback rendering ONLY.
///
/// This is delimiter-less (`<namespace><name>`, no `__`) and therefore NOT a
/// valid flat-wire tool name and NOT reversible. It exists for log/error
/// messages and as one of the decode-fallback spellings the registry indexes
/// (see `build_flat_index`). To build an outbound wire name use
/// [`ToolName::to_wire_name`] / [`flat_mcp_tool_name`], which return a
/// [`WireToolName`]; never route a wire name through this impl.
impl fmt::Display for ToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.namespace {
            Some(namespace) => write!(f, "{namespace}{}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

impl Ord for ToolName {
    fn cmp(&self, other: &Self) -> Ordering {
        let lhs = match &self.namespace {
            Some(namespace) => (namespace.as_str(), Some(self.name.as_str())),
            None => (self.name.as_str(), None),
        };
        let rhs = match &other.namespace {
            Some(namespace) => (namespace.as_str(), Some(other.name.as_str())),
            None => (other.name.as_str(), None),
        };
        lhs.cmp(&rhs)
    }
}

impl PartialOrd for ToolName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl From<String> for ToolName {
    fn from(name: String) -> Self {
        Self::plain(name)
    }
}

impl From<&str> for ToolName {
    fn from(name: &str) -> Self {
        Self::plain(name)
    }
}

#[cfg(test)]
mod flat_mcp_tool_name_tests {
    use super::flat_mcp_tool_name;

    #[test]
    fn namespaced_uses_double_underscore_delimiter() {
        assert_eq!(
            flat_mcp_tool_name("analyze_symbol", Some("mcp__mastra_search")),
            "mcp__mastra_search__analyze_symbol"
        );
    }

    #[test]
    fn namespaced_atlassian_is_not_delimiter_less() {
        // Regression: the broken form was `mcp__atlassianadd_jira_comment`
        // (no `__` between server and tool). Encode must keep the delimiter.
        assert_eq!(
            flat_mcp_tool_name("add_jira_comment", Some("mcp__atlassian")),
            "mcp__atlassian__add_jira_comment"
        );
    }

    #[test]
    fn unnamespaced_is_bare_name() {
        assert_eq!(flat_mcp_tool_name("shell", None), "shell");
    }

    #[test]
    fn empty_namespace_is_bare_name() {
        assert_eq!(flat_mcp_tool_name("shell", Some("")), "shell");
    }
}

#[cfg(test)]
mod wire_tool_name_tests {
    use super::ToolName;

    #[test]
    fn to_wire_name_uses_delimiter_for_namespaced() {
        let wire = ToolName::namespaced("mcp__atlassian", "add_jira_comment").to_wire_name();
        assert_eq!(wire.as_str(), "mcp__atlassian__add_jira_comment");
    }

    #[test]
    fn to_wire_name_is_bare_for_plain() {
        assert_eq!(ToolName::plain("shell").to_wire_name().as_str(), "shell");
    }

    #[test]
    fn wire_name_differs_from_lossy_display() {
        // The compile-time guard's reason for existing: `Display` is the
        // delimiter-less decode-fallback spelling, NOT the wire name. They
        // must NOT be interchangeable for a namespaced tool.
        let name = ToolName::namespaced("mcp__atlassian", "add_jira_comment");
        let display = name.to_string();
        let wire = name.to_wire_name();
        assert_eq!(display, "mcp__atlassianadd_jira_comment");
        assert_eq!(wire.as_str(), "mcp__atlassian__add_jira_comment");
        assert_ne!(display.as_str(), wire.as_str());
    }

    #[test]
    fn advertise_reencode_roundtrip_at_type_boundary() {
        // advertise (to_wire_name) == re-encode (to_wire_name) by construction:
        // the newtype has no other constructor, so the encode path is the only
        // path. A history re-encoder typed to WireToolName cannot accidentally
        // emit the bare `Display` form (it would not type-check).
        let tool = ToolName::namespaced("mcp__mastra_search", "analyze_symbol");
        let advertised = tool.to_wire_name();
        let reencoded = tool.to_wire_name();
        assert_eq!(advertised, reencoded);
        assert_eq!(advertised.as_str(), "mcp__mastra_search__analyze_symbol");
    }
}

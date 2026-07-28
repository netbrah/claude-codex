//! XLI wire extensions — types that must survive upstream `codex-protocol` wholesale.
//!
//! Canonical home for symbols that would be lost on absorb if `protocol/` tracks
//! upstream. `codex-protocol` re-exports for backward compatibility until
//! consumers migrate imports here.

pub mod config;
pub mod token_usage;
pub mod tool_name;

pub use config::ToolChoice;
pub use token_usage::WireTokenUsage;
pub use tool_name::FLAT_MCP_TOOL_NAME_DELIMITER;
pub use tool_name::ToolName;
pub use tool_name::WireToolName;
pub use tool_name::flat_mcp_tool_name;

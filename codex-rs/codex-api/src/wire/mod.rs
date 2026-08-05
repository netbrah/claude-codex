//! Cross-wire emission invariants ported from LiteLLM (S-LITELLM-BRIDGE-HARDENING).

pub mod openai_strict_schema;
pub mod tool_name;

pub use openai_strict_schema::ensure_openai_strict;
pub use tool_name::OPENAI_MAX_TOOL_NAME_LENGTH;
pub use tool_name::ToolNameMapping;
pub use tool_name::restore_original_tool_name;
pub use tool_name::restore_original_tool_names_in_item;
pub use tool_name::tool_name_mapping;
pub use tool_name::truncate_tool_name;

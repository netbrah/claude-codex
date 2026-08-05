//! `codex-copilot-host` — Rust host for GitHub Copilot CLI style extensions.
//!
//! This crate lets XLI fork and drive extension processes that were authored
//! against `@github/copilot-sdk`. The extension surface is:
//!
//! * Discovery scans `<workspace>/.github/extensions/*/extension.mjs` plus an
//!   optional user-global directory.
//! * Each extension is spawned as `node <path>/extension.mjs` with
//!   `SESSION_ID` in the environment and stdio pipes for a framed JSON-RPC 2.0
//!   (`vscode-jsonrpc`) transport.
//! * The host plays the Copilot-CLI parent role: it answers `session.resume`,
//!   `ping`, and `tools.list` at minimum, and forwards `tool.call` requests
//!   back to the child when the agent picks an extension-registered tool.
//!
//! The crate is deliberately narrow on the engine side — it does not know
//! about `ResponseItem`s, models, or the TUI. It just manages the wire.
//! Integration with the Codex core happens through [`ExtensionHost::tools`]
//! plus the [`ExtensionHost::invoke_tool`] hook, which other crates can
//! bridge into their own tool dispatch.

pub mod discovery;
pub mod extension;
pub mod framing;
pub mod host;
pub mod protocol;

pub use discovery::DiscoveredExtension;
pub use discovery::discover_extensions;
pub use extension::ExtensionHandle;
pub use extension::ExtensionOptions;
pub use framing::FramingError;
pub use framing::read_message;
pub use framing::write_message;
pub use host::ExtensionHost;
pub use host::ExtensionHostConfig;
pub use host::ToolDescriptor;
pub use protocol::JsonRpcError;
pub use protocol::JsonRpcMessage;
pub use protocol::JsonRpcRequest;
pub use protocol::JsonRpcResponse;

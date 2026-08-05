---
name: mcp-server-and-tools
description: Use when working in codex-rs/codex-mcp/, codex-rs/mcp-server/, codex-rs/rmcp-client/, or codex-rs/copilot-host/. Owns MCP connection management, tool registration/dispatch, the mcp-server binary (stdio JSON-RPC), rmcp client transport, and the copilot-host extension system.
allowed-tools: Read, Edit, Grep, Bash(cargo nextest run -p codex-mcp *), Bash(cargo nextest run -p mcp-server *)
---

# MCP server, tools, and copilot-host

## When to load
Any diff under `codex-rs/codex-mcp/`, `codex-rs/mcp-server/`,
`codex-rs/rmcp-client/`, or `codex-rs/copilot-host/`.

## Crate map

### codex-mcp (connection manager)
| File | Role |
|------|------|
| `src/mcp_connection_manager.rs` | Core manager (~64KB) — connection lifecycle, tool listing, caching |
| `src/mcp_tool_names.rs` | Tool name normalization (`normalize_codex_apps_callable_name()`) |
| `src/mcp/mod.rs` | `McpManager` type alias + submodules |
| `src/mcp/auth.rs` | MCP auth handling |
| `src/mcp/skill_dependencies.rs` | Skill-to-MCP dependency resolution |

Tool registration flows through `list_tools_for_client_uncached()` which:
1. Calls `client.list_tools_with_connector_ids()`
2. Normalizes tool names/namespaces
3. Wraps each in `ToolInfo`
4. Caches to disk via `write_cached_codex_apps_tools()` / `read_cached_codex_apps_tools()`

### mcp-server (stdio binary)
| File | Role |
|------|------|
| `src/lib.rs` | `run_main()` entry — loads config, builds MessageProcessor, stdio JSON-RPC loop |
| `src/message_processor.rs` | Request dispatch (Initialize, Ping, ListTools, CallTool) |
| `src/codex_tool_config.rs` | Tool configuration |
| `src/codex_tool_runner.rs` | Spawns full Codex threads for tool execution |
| `src/exec_approval.rs` | Exec approval handling |
| `src/patch_approval.rs` | Patch approval handling |

Exposes two tools:
- `"codex"` → `handle_tool_call_codex()` (spawns a full Codex thread)
- `"codex-reply"` → `handle_tool_call_codex_session_reply()` (reply to existing session)

### rmcp-client (transport layer)
| File | Role |
|------|------|
| `src/rmcp_client.rs` | Main client (~43KB) — `RmcpClient`, `Elicitation`, `SendElicitation` |
| `src/oauth.rs` | OAuth token management (~31KB) |
| `src/perform_oauth_login.rs` | OAuth login flow (~20KB) |
| `src/auth_status.rs` | `determine_streamable_http_auth_status()` |
| `src/program_resolver.rs` | MCP server program discovery |

### copilot-host (extension host — MVP)
Rust host for GitHub Copilot CLI-style extensions. This is the **inverse**
direction from the copilot adapter — XLI as **host** for extensions, not as
a Copilot client.

| File | Role |
|------|------|
| `src/discovery.rs` | Scans `.github/extensions/*/extension.mjs` |
| `src/extension.rs` | `ExtensionHandle` — fork + stdio management |
| `src/framing.rs` | `Content-Length` vscode-jsonrpc framing |
| `src/host.rs` | `ExtensionHost`, `ToolDescriptor`, method dispatch |
| `src/protocol.rs` | JSON-RPC 2.0 types |

Key types: `ExtensionHost`, `ExtensionHostConfig`, `ToolDescriptor`,
`DiscoveredExtension`, `ExtensionHandle`, `ExtensionOptions`

Wire methods: `session.resume` (inbound), `tool.call` (outbound), `ping`, `tools.list`

**Status**: MVP sortie. Not yet wired into `codex-core` agent tool dispatch.

## Workflow
1. Read the existing registration pattern before adding new tools or connections.
2. For MCP tools: define JSON Schema, implement handler, register in processor.
3. For copilot-host: extensions are discovered via filesystem scan, not explicit registration.
4. `cargo nextest run -p codex-mcp -p mcp-server` green.

## Anti-patterns
- Registering tools with vague descriptions (the model needs clear schemas).
- Bypassing MCP dispatch to call tool handlers directly from core.
- Hardcoding transport assumptions (stdio vs SSE vs streamable-HTTP).
- Confusing copilot-host (extension host) with copilot adapter (model client).

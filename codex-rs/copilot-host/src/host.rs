//! High-level orchestration: discovery → spawn N extensions → manage tool
//! registry → dispatch tool calls.
//!
//! The host implements just enough of the Copilot-CLI parent role to let a
//! real `@github/copilot-sdk` extension handshake via `joinSession()` and
//! register tools. When the agent picks one of those tools, the host calls
//! back into the child with `tool.call`.
//!
//! What the host currently handles inbound from children:
//! * `session.resume` — returns capabilities + workspace; captures registered
//!   tools/commands into the host registry.
//! * `ping` — echo.
//! * `tools.list` — returns the concatenation of every child's registered
//!   tools.
//!
//! Everything else is replied to with JSON-RPC `-32601 Method not found` so
//! we can see on wire which methods the SDK actually uses for a given flow.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;
use serde_json::json;
use tokio::sync::RwLock;

use crate::discovery::DiscoveredExtension;
use crate::discovery::discover_extensions;
use crate::extension::ExtensionError;
use crate::extension::ExtensionHandle;
use crate::extension::ExtensionOptions;
use crate::extension::invalid_params;
use crate::extension::method_not_found;
use crate::protocol::JsonRpcError;
use crate::protocol::SessionCapabilities;
use crate::protocol::SessionResumeParams;
use crate::protocol::SessionResumeResult;
use crate::protocol::ToolRegistration;

/// How the host should fork extensions.
#[derive(Debug, Clone)]
pub struct ExtensionHostConfig {
    pub workspace: PathBuf,
    pub user_extensions_dir: Option<PathBuf>,
    pub session_id: String,
    pub node_binary: PathBuf,
    pub client_name: String,
    pub protocol_version: u32,
    /// Optional bootstrap script used as the real `node` entrypoint. When set,
    /// children are forked as `node <bootstrap> ` with `EXTENSION_PATH` and
    /// `COPILOT_SDK_PATH` pointing at the extension's `.mjs` and the bundled
    /// SDK respectively. When `None`, the extension `.mjs` is executed
    /// directly — useful for extensions that bundle their own SDK.
    pub bootstrap: Option<PathBuf>,
    /// Directory containing the bundled `@github/copilot-sdk` ESM files
    /// (`index.js`, `extension.js`). Only consulted when `bootstrap` is set.
    pub sdk_path: Option<PathBuf>,
}

impl ExtensionHostConfig {
    pub fn new(workspace: PathBuf, session_id: impl Into<String>) -> Self {
        Self {
            workspace,
            user_extensions_dir: None,
            session_id: session_id.into(),
            node_binary: PathBuf::from("node"),
            client_name: "xli-copilot-host".to_string(),
            protocol_version: 1,
            bootstrap: None,
            sdk_path: None,
        }
    }
}

/// A tool registered by some extension. `owner` is the extension id that
/// dispatches this tool.
#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    pub owner: String,
    pub name: String,
    pub description: Option<String>,
    pub parameters: Option<Value>,
    pub skip_permission: bool,
}

#[derive(Default)]
struct State {
    /// Tool registry keyed by tool name. Last writer wins on conflict — this
    /// matches Copilot-CLI's "later extension can overrideBuiltInTool"
    /// behavior.
    tools: HashMap<String, ToolDescriptor>,
    /// Handles by extension id.
    extensions: HashMap<String, Arc<ExtensionHandle>>,
}

#[derive(Clone)]
pub struct ExtensionHost {
    cfg: ExtensionHostConfig,
    state: Arc<RwLock<State>>,
}

impl ExtensionHost {
    pub fn new(cfg: ExtensionHostConfig) -> Self {
        Self {
            cfg,
            state: Arc::new(RwLock::new(State::default())),
        }
    }

    /// Discover + spawn all extensions. Failures on a single extension are
    /// logged and the remaining extensions continue.
    pub async fn start(&self) -> anyhow::Result<Vec<DiscoveredExtension>> {
        let discovered =
            discover_extensions(&self.cfg.workspace, self.cfg.user_extensions_dir.as_deref())
                .await?;

        for d in &discovered {
            if let Err(err) = self.spawn_extension(d).await {
                tracing::warn!(
                    extension = %d.id,
                    "failed to spawn extension: {err}"
                );
            }
        }

        Ok(discovered)
    }

    pub async fn spawn_extension(
        &self,
        d: &DiscoveredExtension,
    ) -> Result<Arc<ExtensionHandle>, ExtensionError> {
        let me = self.clone();
        let ext_id_for_handler = d.id.clone();

        let on_request = Arc::new(
            move |method: String,
                  params: Option<Value>|
                  -> BoxFuture<'static, Result<Value, JsonRpcError>> {
                let me = me.clone();
                let ext_id = ext_id_for_handler.clone();
                Box::pin(async move { me.handle_child_request(&ext_id, &method, params).await })
            },
        );

        let mut extra_env: Vec<(String, String)> = Vec::new();
        extra_env.push((
            "EXTENSION_PATH".to_string(),
            d.entry.to_string_lossy().into_owned(),
        ));
        if let Some(sdk) = &self.cfg.sdk_path {
            extra_env.push((
                "COPILOT_SDK_PATH".to_string(),
                sdk.to_string_lossy().into_owned(),
            ));
        }

        // If a bootstrap is configured, it becomes the real entrypoint and
        // the real extension path travels in `EXTENSION_PATH`. Otherwise the
        // extension `.mjs` is exec'd directly.
        let entry = match &self.cfg.bootstrap {
            Some(b) => b.clone(),
            None => d.entry.clone(),
        };

        let opts = ExtensionOptions {
            id: d.id.clone(),
            entry,
            session_id: self.cfg.session_id.clone(),
            node_binary: self.cfg.node_binary.clone(),
            extra_env,
            cwd: d.entry.parent().map(Path::to_path_buf),
        };
        let handle = Arc::new(ExtensionHandle::spawn(opts, on_request, None).await?);
        let mut state = self.state.write().await;
        state.extensions.insert(d.id.clone(), handle.clone());
        Ok(handle)
    }

    /// Flattened view of every tool known to the host.
    pub async fn tools(&self) -> Vec<ToolDescriptor> {
        let s = self.state.read().await;
        let mut out: Vec<ToolDescriptor> = s.tools.values().cloned().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Invoke a registered tool on its owning extension. The return value is
    /// whatever the child produced in its `tool.call` result field.
    pub async fn invoke_tool(&self, tool_name: &str, arguments: Value) -> anyhow::Result<Value> {
        let (owner_handle, params) = {
            let s = self.state.read().await;
            let desc = s
                .tools
                .get(tool_name)
                .ok_or_else(|| anyhow::anyhow!("unknown tool: {tool_name}"))?;
            let handle = s
                .extensions
                .get(&desc.owner)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("owner {} no longer running", desc.owner))?;
            let params = json!({
                "sessionId": self.cfg.session_id,
                "toolName": tool_name,
                "arguments": arguments,
            });
            (handle, params)
        };
        let result = owner_handle.send_request("tool.call", Some(params)).await?;
        Ok(result)
    }

    /// Shut every extension down cleanly.
    pub async fn shutdown(&self) {
        let extensions: Vec<Arc<ExtensionHandle>> = {
            let s = self.state.read().await;
            s.extensions.values().cloned().collect()
        };
        for h in extensions {
            h.shutdown().await;
        }
        let mut s = self.state.write().await;
        s.extensions.clear();
        s.tools.clear();
    }

    // -------------------------------------------------------------------------
    // Inbound routing
    // -------------------------------------------------------------------------
    async fn handle_child_request(
        &self,
        owner: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, JsonRpcError> {
        match method {
            "ping" => Ok(json!({
                "protocolVersion": self.cfg.protocol_version,
                "message": params
                    .as_ref()
                    .and_then(|v| v.get("message"))
                    .cloned()
                    .unwrap_or(Value::Null),
            })),

            "session.resume" => self.handle_session_resume(owner, params).await,

            "tools.list" => Ok(json!({
                "tools": self
                    .tools()
                    .await
                    .into_iter()
                    .map(|t| json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }))
                    .collect::<Vec<_>>(),
            })),

            other => Err(method_not_found(other)),
        }
    }

    async fn handle_session_resume(
        &self,
        owner: &str,
        params: Option<Value>,
    ) -> Result<Value, JsonRpcError> {
        let params = params.ok_or_else(|| invalid_params("session.resume requires params"))?;
        let parsed: SessionResumeParams = serde_json::from_value(params)
            .map_err(|e| invalid_params(format!("session.resume params: {e}")))?;

        if parsed.session_id != self.cfg.session_id {
            return Err(invalid_params(format!(
                "session.resume: expected sessionId {}, got {}",
                self.cfg.session_id, parsed.session_id
            )));
        }

        if let Some(tools) = parsed.tools {
            let mut s = self.state.write().await;
            for t in tools {
                let ToolRegistration {
                    name,
                    description,
                    parameters,
                    skip_permission,
                    ..
                } = t;
                s.tools.insert(
                    name.clone(),
                    ToolDescriptor {
                        owner: owner.to_string(),
                        name,
                        description,
                        parameters,
                        skip_permission: skip_permission.unwrap_or(false),
                    },
                );
            }
        }

        let result = SessionResumeResult {
            workspace_path: self.cfg.workspace.to_string_lossy().to_string(),
            capabilities: SessionCapabilities {
                protocol_version: self.cfg.protocol_version,
                negotiated_protocol_version: self.cfg.protocol_version,
                streaming: true,
                vision: false,
            },
        };
        serde_json::to_value(result).map_err(|e| invalid_params(e.to_string()))
    }
}

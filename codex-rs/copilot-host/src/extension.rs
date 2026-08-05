//! One forked extension process + its framed JSON-RPC wire.
//!
//! Responsibilities:
//! * `node <entry>` spawn with `SESSION_ID` env, piped stdio.
//! * Reader task: decodes framed JSON-RPC from child stdout, routes each
//!   message (request / response / notification) onto channels.
//! * Writer task: accepts outbound messages and frames them to child stdin.
//! * `send_request` correlator: stores pending responders keyed by request id.
//! * Graceful shutdown: SIGTERM, then SIGKILL after 5s (matching the Copilot
//!   CLI contract documented in the SDK).
//!
//! The extension does not know about tool dispatch directly; it hands every
//! inbound request to the caller-supplied [`HostRequestHandler`]. The
//! [`ExtensionHost`](crate::host::ExtensionHost) owns that handler and turns
//! it into a real tool/permission/event router.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::debug;
use tracing::warn;

use crate::framing::FramingError;
use crate::framing::{self};
use crate::protocol::JsonRpcError;
use crate::protocol::JsonRpcMessage;
use crate::protocol::JsonRpcNotification;
use crate::protocol::JsonRpcRequest;
use crate::protocol::JsonRpcResponse;
use crate::protocol::JsonRpcVersion;
use crate::protocol::error_codes;

/// Configuration for a single forked extension.
#[derive(Debug, Clone)]
pub struct ExtensionOptions {
    pub id: String,
    pub entry: PathBuf,
    pub session_id: String,
    /// Process that launches the extension. Defaults to `node`.
    pub node_binary: PathBuf,
    /// Extra env pairs (session-scoped) added on top of the inherited env.
    pub extra_env: Vec<(String, String)>,
    /// Working directory for the child. Defaults to the extension's own dir.
    pub cwd: Option<PathBuf>,
}

impl ExtensionOptions {
    pub fn new(id: impl Into<String>, entry: PathBuf, session_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            entry,
            session_id: session_id.into(),
            node_binary: PathBuf::from("node"),
            extra_env: Vec::new(),
            cwd: None,
        }
    }
}

/// Handler invoked for every inbound request from the child.
///
/// The host's routing table is the `&ExtensionHost` captured by closure when
/// [`ExtensionHandle::spawn`] is called.
pub type HostRequestHandler = Arc<
    dyn Fn(
            String,
            Option<Value>,
        ) -> futures::future::BoxFuture<'static, Result<Value, JsonRpcError>>
        + Send
        + Sync,
>;

/// Handler for inbound notifications (e.g. child-side logs). Optional.
pub type HostNotificationHandler = Arc<dyn Fn(String, Option<Value>) + Send + Sync>;

#[derive(Debug, Error)]
pub enum ExtensionError {
    #[error("extension {0} exited before handshake")]
    EarlyExit(String),
    #[error("extension {id} wire error: {source}")]
    Wire {
        id: String,
        #[source]
        source: FramingError,
    },
    #[error("extension {id} returned error: {code} {message}")]
    RemoteError {
        id: String,
        code: i64,
        message: String,
    },
    #[error("extension {id} response for request {req_id} timed out")]
    ResponseTimeout { id: String, req_id: i64 },
    #[error("extension {id} shut down")]
    Shutdown { id: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Handle to a running extension. Drop does not kill the child — call
/// [`ExtensionHandle::shutdown`] explicitly so the SIGTERM→SIGKILL cadence is
/// deterministic.
pub struct ExtensionHandle {
    id: String,
    outbound_tx: mpsc::Sender<OutboundMessage>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<JsonRpcResponse>>>>,
    next_id: Arc<AtomicI64>,
    reader_task: Mutex<Option<JoinHandle<()>>>,
    writer_task: Mutex<Option<JoinHandle<()>>>,
    child: Arc<Mutex<Child>>,
}

enum OutboundMessage {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

impl ExtensionHandle {
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Spawn the extension, start the reader/writer tasks, return a handle.
    pub async fn spawn(
        opts: ExtensionOptions,
        on_request: HostRequestHandler,
        on_notification: Option<HostNotificationHandler>,
    ) -> Result<Self, ExtensionError> {
        let cwd = opts
            .cwd
            .clone()
            .or_else(|| opts.entry.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        let mut cmd = Command::new(&opts.node_binary);
        cmd.arg(&opts.entry)
            .current_dir(&cwd)
            .env("SESSION_ID", &opts.session_id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &opts.extra_env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| {
            tracing::warn!(
                extension = %opts.id,
                "spawn failed: node={:?} entry={:?} err={e}",
                opts.node_binary, opts.entry,
            );
            ExtensionError::Io(e)
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ExtensionError::EarlyExit(opts.id.clone()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ExtensionError::EarlyExit(opts.id.clone()))?;
        let stderr = child.stderr.take();

        // Drain stderr to tracing so it doesn't block the child.
        if let Some(stderr) = stderr {
            let id_for_log = opts.id.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let mut r = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = r.next_line().await {
                    debug!(target: "copilot_host::ext_stderr", extension = %id_for_log, "{line}");
                }
            });
        }

        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<JsonRpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<OutboundMessage>(64);
        let next_id = Arc::new(AtomicI64::new(1));

        // Writer task.
        let writer_task = {
            let id = opts.id.clone();
            tokio::spawn(async move {
                let mut stdin = stdin;
                while let Some(msg) = outbound_rx.recv().await {
                    let payload = match &msg {
                        OutboundMessage::Request(r) => serde_json::to_vec(r),
                        OutboundMessage::Response(r) => serde_json::to_vec(r),
                        OutboundMessage::Notification(n) => serde_json::to_vec(n),
                    };
                    let payload = match payload {
                        Ok(p) => p,
                        Err(err) => {
                            warn!(extension = %id, "failed to serialize outbound: {err}");
                            continue;
                        }
                    };
                    if let Err(err) = framing::write_message(&mut stdin, &payload).await {
                        warn!(extension = %id, "writer halted: {err}");
                        break;
                    }
                }
            })
        };

        // Reader task.
        let reader_task = {
            let id = opts.id.clone();
            let pending = pending.clone();
            let outbound_tx_for_reader = outbound_tx.clone();
            let on_request = on_request.clone();
            let on_notification = on_notification.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                loop {
                    let buf = match framing::read_message(&mut reader).await {
                        Ok(b) => b,
                        Err(FramingError::Eof) => {
                            debug!(extension = %id, "child closed stdout");
                            break;
                        }
                        Err(err) => {
                            warn!(extension = %id, "reader halted: {err}");
                            break;
                        }
                    };

                    let msg: JsonRpcMessage = match serde_json::from_slice(&buf) {
                        Ok(m) => m,
                        Err(err) => {
                            warn!(extension = %id, "invalid json: {err}; payload={}", String::from_utf8_lossy(&buf));
                            continue;
                        }
                    };

                    match msg {
                        JsonRpcMessage::Response(resp) => {
                            if let Some(rid) = value_to_i64(&resp.id) {
                                let mut p = pending.lock().await;
                                if let Some(tx) = p.remove(&rid) {
                                    let _ = tx.send(resp);
                                } else {
                                    warn!(extension = %id, "response for unknown id {rid}");
                                }
                            } else {
                                warn!(extension = %id, "response with non-integer id ignored: {:?}", resp.id);
                            }
                        }
                        JsonRpcMessage::Request(req) => {
                            let outbound = outbound_tx_for_reader.clone();
                            let on_request = on_request.clone();
                            let id_for_req = id.clone();
                            tokio::spawn(async move {
                                let resp = match (on_request)(
                                    req.method.clone(),
                                    req.params.clone(),
                                )
                                .await
                                {
                                    Ok(v) => JsonRpcResponse {
                                        jsonrpc: JsonRpcVersion::V2_0,
                                        id: req.id,
                                        result: Some(v),
                                        error: None,
                                    },
                                    Err(err) => JsonRpcResponse {
                                        jsonrpc: JsonRpcVersion::V2_0,
                                        id: req.id,
                                        result: None,
                                        error: Some(err),
                                    },
                                };
                                if let Err(e) = outbound.send(OutboundMessage::Response(resp)).await
                                {
                                    warn!(extension = %id_for_req, "could not enqueue response: {e}");
                                }
                            });
                        }
                        JsonRpcMessage::Notification(n) => {
                            if let Some(h) = &on_notification {
                                (h)(n.method, n.params);
                            } else {
                                debug!(extension = %id, "notification {}", n.method);
                            }
                        }
                    }
                }
            })
        };

        Ok(Self {
            id: opts.id,
            outbound_tx,
            pending,
            next_id,
            reader_task: Mutex::new(Some(reader_task)),
            writer_task: Mutex::new(Some(writer_task)),
            child: Arc::new(Mutex::new(child)),
        })
    }

    /// Send a typed JSON-RPC request to the child and await the response.
    pub async fn send_request(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, ExtensionError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut p = self.pending.lock().await;
            p.insert(id, tx);
        }
        let req = JsonRpcRequest {
            jsonrpc: JsonRpcVersion::V2_0,
            id: Value::from(id),
            method: method.to_string(),
            params,
        };
        self.outbound_tx
            .send(OutboundMessage::Request(req))
            .await
            .map_err(|_| ExtensionError::Shutdown {
                id: self.id.clone(),
            })?;

        let resp = match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => {
                return Err(ExtensionError::Shutdown {
                    id: self.id.clone(),
                });
            }
            Err(_) => {
                let mut p = self.pending.lock().await;
                p.remove(&id);
                return Err(ExtensionError::ResponseTimeout {
                    id: self.id.clone(),
                    req_id: id,
                });
            }
        };

        if let Some(err) = resp.error {
            return Err(ExtensionError::RemoteError {
                id: self.id.clone(),
                code: err.code,
                message: err.message,
            });
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }

    /// Send a notification to the child. Fire and forget.
    pub async fn send_notification(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), ExtensionError> {
        let note = JsonRpcNotification {
            jsonrpc: JsonRpcVersion::V2_0,
            method: method.to_string(),
            params,
        };
        self.outbound_tx
            .send(OutboundMessage::Notification(note))
            .await
            .map_err(|_| ExtensionError::Shutdown {
                id: self.id.clone(),
            })?;
        Ok(())
    }

    /// SIGTERM, then SIGKILL after `5s`, matching the Copilot CLI contract.
    pub async fn shutdown(&self) {
        let mut child = self.child.lock().await;

        // Try to close stdin by dropping outbound channel side effects isn't
        // enough; we just ask the kernel to signal.
        #[cfg(unix)]
        {
            if let Some(pid) = child.id() {
                unsafe {
                    libc_sigterm(pid as i32);
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = child.start_kill();
        }

        let wait = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        if wait.is_err() {
            debug!(extension = %self.id, "SIGTERM timed out, sending SIGKILL");
            let _ = child.start_kill();
            let _ = child.wait().await;
        }

        drop(child);

        // Release reader/writer tasks.
        if let Some(h) = self.reader_task.lock().await.take() {
            let _ = h.await;
        }
        if let Some(h) = self.writer_task.lock().await.take() {
            h.abort();
            let _ = h.await;
        }
    }
}

fn value_to_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

#[cfg(unix)]
unsafe fn libc_sigterm(pid: i32) {
    // Avoid pulling the `libc` crate in: use raw syscall via nix-free FFI.
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    const SIGTERM: i32 = 15;
    unsafe {
        kill(pid, SIGTERM);
    }
}

/// Conveniences for building a [`JsonRpcError`] from closures in the host
/// request handler.
pub fn method_not_found(method: &str) -> JsonRpcError {
    JsonRpcError {
        code: error_codes::METHOD_NOT_FOUND,
        message: format!("Method not found: {method}"),
        data: None,
    }
}

pub fn invalid_params(msg: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code: error_codes::INVALID_PARAMS,
        message: msg.into(),
        data: None,
    }
}

pub fn internal_error(msg: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code: error_codes::INTERNAL_ERROR,
        message: msg.into(),
        data: None,
    }
}

//! Test-only HTTP server for gated SSE delivery.
//!
//! Wiremock writes the full response body before closing the socket, which is
//! fine for batch assertions but cannot prove progressive streaming — a
//! client may buffer the whole body in memory and still pass wiremock-based
//! assertions. This server emits byte chunks one at a time, under explicit
//! test control, so we can assert:
//!
//!   * Event N is delivered to the consumer BEFORE chunk N+1 is sent.
//!   * A stall (server stops sending after chunk K) surfaces as an error to
//!     the consumer within a bounded deadline instead of hanging forever.
//!
//! Pattern mirrors `codex-rs/core/tests/common/streaming_sse.rs` but trimmed
//! to the Copilot test surface: `GET /copilot_internal/v2/token` for the JWT
//! exchange and `POST /chat/completions` for the SSE stream.

#![allow(dead_code)]

use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::oneshot;

/// One SSE chunk with an optional release gate. The server awaits `gate`
/// (if present) before writing `body` to the socket. Tests use gates to
/// force a specific delivery order.
pub struct GatedChunk {
    pub gate: Option<oneshot::Receiver<()>>,
    pub body: String,
}

impl GatedChunk {
    pub fn immediate(body: impl Into<String>) -> Self {
        Self {
            gate: None,
            body: body.into(),
        }
    }

    pub fn gated(body: impl Into<String>) -> (oneshot::Sender<()>, Self) {
        let (tx, rx) = oneshot::channel();
        (
            tx,
            Self {
                gate: Some(rx),
                body: body.into(),
            },
        )
    }
}

pub struct GatedCopilotServer {
    uri: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl GatedCopilotServer {
    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

pub struct GatedResponseSpec {
    pub jwt_body: String,
    pub chat_chunks: Vec<GatedChunk>,
    /// If true, stops writing after the gated chunks without closing the
    /// socket cleanly (simulates a stall). If false, shuts down the write
    /// half after the final chunk (simulates a clean end-of-stream).
    pub stall_after_chunks: bool,
}

/// Start a single-shot gated server. Serves one JWT GET, one chat POST, then
/// the remaining connections after chat close get rejected. Tests that need
/// multiple chat POSTs should clone the spec list.
pub async fn start_gated_server(spec: GatedResponseSpec) -> GatedCopilotServer {
    start_gated_server_with(|_uri| spec).await
}

/// Bind the listener first, then build the spec with knowledge of the
/// server's actual URI. Tests use this so the JWT envelope's
/// `endpoints.api` matches where the chat POST will land.
pub async fn start_gated_server_with<F>(build: F) -> GatedCopilotServer
where
    F: FnOnce(&str) -> GatedResponseSpec,
{
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind gated server");
    let addr = listener.local_addr().expect("local addr");
    let uri = format!("http://{addr}");

    let spec = build(&uri);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let spec = Arc::new(TokioMutex::new(Some(spec)));

    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accept = listener.accept() => {
                    let Ok((mut stream, _)) = accept else { continue };
                    let spec = Arc::clone(&spec);
                    tokio::spawn(async move {
                        handle_connection(&mut stream, spec).await;
                    });
                }
            }
        }
    });

    GatedCopilotServer {
        uri,
        shutdown: Some(shutdown_tx),
        task: Some(task),
    }
}

async fn handle_connection(
    stream: &mut TcpStream,
    spec: Arc<TokioMutex<Option<GatedResponseSpec>>>,
) {
    let (head, body_prefix) = read_headers(stream).await;
    let Some((method, path)) = parse_request_line(&head) else {
        let _ = write_status(stream, 400, "bad request").await;
        return;
    };

    if method == "GET" && path.starts_with("/copilot_internal/v2/token") {
        let jwt_body = {
            let guard = spec.lock().await;
            guard.as_ref().map(|s| s.jwt_body.clone())
        };
        match jwt_body {
            Some(body) => {
                let _ = write_json(stream, 200, &body).await;
            }
            None => {
                let _ = write_status(stream, 500, "spec consumed").await;
            }
        }
        return;
    }

    if method == "POST" && path == "/chat/completions" {
        // Drain request body if any, accounting for bytes already read into
        // `body_prefix` while parsing headers.
        let _ = drain_body(stream, &head, body_prefix.len()).await;

        let spec_opt = { spec.lock().await.take() };
        let Some(spec) = spec_opt else {
            let _ = write_status(stream, 500, "spec consumed").await;
            return;
        };

        if write_sse_headers(stream).await.is_err() {
            return;
        }

        for chunk in spec.chat_chunks {
            if let Some(gate) = chunk.gate
                && gate.await.is_err()
            {
                return;
            }
            if stream.write_all(chunk.body.as_bytes()).await.is_err() {
                return;
            }
            let _ = stream.flush().await;
        }

        if !spec.stall_after_chunks {
            let _ = stream.shutdown().await;
        } else {
            // Intentionally hold the socket open — caller relies on a
            // client-side timeout to surface the stall.
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
        return;
    }

    let _ = write_status(stream, 404, "not found").await;
}

async fn read_headers(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 1024];
    loop {
        let n = match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        buf.extend_from_slice(&tmp[..n]);
        if let Some(idx) = find_subslice(&buf, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..idx]).to_string();
            let body_prefix = buf[idx + 4..].to_vec();
            return (head, body_prefix);
        }
    }
    (String::from_utf8_lossy(&buf).to_string(), Vec::new())
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn parse_request_line(head: &str) -> Option<(&str, &str)> {
    let first = head.lines().next()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;
    Some((method, path))
}

async fn drain_body(
    stream: &mut TcpStream,
    head: &str,
    already_buffered: usize,
) -> tokio::io::Result<()> {
    let mut content_length: usize = 0;
    for line in head.lines() {
        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:")
            && let Ok(n) = rest.trim().parse::<usize>()
        {
            content_length = n;
        }
    }
    if content_length == 0 {
        return Ok(());
    }
    let mut remaining = content_length.saturating_sub(already_buffered);
    let mut scratch = [0u8; 2048];
    while remaining > 0 {
        let n = stream.read(&mut scratch).await?;
        if n == 0 {
            break;
        }
        remaining = remaining.saturating_sub(n);
    }
    Ok(())
}

async fn write_sse_headers(stream: &mut TcpStream) -> tokio::io::Result<()> {
    let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
    stream.write_all(headers.as_bytes()).await?;
    stream.flush().await
}

async fn write_json(stream: &mut TcpStream, status: u16, body: &str) -> tokio::io::Result<()> {
    // Send the JSON response with `Connection: close` so reqwest's pool does
    // not attempt to reuse the socket for the follow-up chat POST. Reusing
    // the socket would race with our per-connection task drop and surface
    // as `IncompleteMessage` on the client.
    let resp = format!(
        "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {len}\r\nconnection: close\r\n\r\n{body}",
        len = body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await?;
    stream.shutdown().await
}

async fn write_status(stream: &mut TcpStream, status: u16, body: &str) -> tokio::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/plain\r\ncontent-length: {len}\r\n\r\n{body}",
        reason = if status == 200 { "OK" } else { "ERR" },
        len = body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await?;
    stream.shutdown().await
}

/// Build a standard `"data: <json>\n"` SSE line. No `[DONE]` terminator —
/// callers append that via `done_sentinel()` when needed.
pub fn sse_line(json: &str) -> String {
    format!("data: {json}\n")
}

pub fn done_sentinel() -> String {
    "data: [DONE]\n".to_string()
}

/// Sample JWT envelope — override `api` to match the server's own URI so the
/// subsequent chat POST lands on the same host.
pub fn jwt_envelope(api_base: &str, exp_offset_secs: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    serde_json::json!({
        "token": "jwt_fake_gated",
        "expires_at": now + exp_offset_secs,
        "refresh_in": 1500,
        "endpoints": {
            "api": api_base,
            "telemetry": null,
            "proxy": null,
        }
    })
    .to_string()
}

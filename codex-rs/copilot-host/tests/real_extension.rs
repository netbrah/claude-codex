//! End-to-end integration test.
//!
//! Spawns a real Node child that frames vscode-jsonrpc on stdio, registers a
//! tool through `session.resume`, and answers `tool.call`. The host drives
//! discovery → fork → registry → tool invocation → clean shutdown.
//!
//! Requires `node` on PATH. Skipped gracefully when `node` is not available
//! so the crate still builds cleanly on minimal CI images.

use std::path::PathBuf;
use std::process::Command;

use codex_copilot_host::ExtensionHost;
use codex_copilot_host::ExtensionHostConfig;
use tempfile::tempdir;

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drives_a_real_node_extension_end_to_end() {
    if !node_available() {
        eprintln!("node not on PATH, skipping");
        return;
    }

    // Build a synthetic workspace: .github/extensions/echo/extension.mjs →
    // symlink to our fixture so discovery works unmodified.
    let ws = tempdir().unwrap();
    let dst_dir = ws.path().join(".github/extensions/echo");
    tokio::fs::create_dir_all(&dst_dir).await.unwrap();
    let src = fixture("echo-ext/extension.mjs");
    let dst = dst_dir.join("extension.mjs");
    tokio::fs::copy(&src, &dst).await.unwrap();

    let mut cfg = ExtensionHostConfig::new(ws.path().to_path_buf(), "sess-xli-test-1");
    cfg.bootstrap = Some(fixture("bootstrap.mjs"));

    let host = ExtensionHost::new(cfg);
    let discovered = host.start().await.expect("host start");
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].id, "echo");

    // Wait for the child's session.resume handshake to propagate into the
    // host tool registry. Poll for up to 5s.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let tools = loop {
        let t = host.tools().await;
        if !t.is_empty() || std::time::Instant::now() >= deadline {
            break t;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(
        tools.len(),
        1,
        "expected echo tool registered, got {tools:?}"
    );
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[0].owner, "echo");
    assert_eq!(
        tools[0].description.as_deref(),
        Some("Echo the text argument back (XLI fixture).")
    );

    // Round-trip a tool.call.
    let out = host
        .invoke_tool("echo", serde_json::json!({ "text": "hello from xli" }))
        .await
        .expect("invoke_tool");

    let content = out
        .get("content")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .expect("expected content[0].text");
    assert_eq!(content, "echo: hello from xli");

    host.shutdown().await;
}

#[tokio::test]
async fn discovery_happy_path_without_node() {
    // Exercises the discovery layer alone so this test always runs.
    let ws = tempdir().unwrap();
    let base = ws.path().join(".github/extensions/alpha");
    tokio::fs::create_dir_all(&base).await.unwrap();
    tokio::fs::write(base.join("extension.mjs"), b"export {};")
        .await
        .unwrap();

    let cfg = ExtensionHostConfig::new(ws.path().to_path_buf(), "sess-2");
    let host = ExtensionHost::new(cfg);

    // With no bootstrap + a tiny no-op extension, spawn_extension succeeds
    // only if `node` is installed. We bypass spawn and just check discovery.
    let items = codex_copilot_host::discover_extensions(ws.path(), None)
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "alpha");
    // Keep the host alive through shutdown without spawning anything.
    host.shutdown().await;
}

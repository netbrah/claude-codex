//! Progressive streaming + stall-surfacing tests for the adapter.
//!
//! Asserts two properties the old (materialize-then-map) design violated:
//!
//! 1. **Progressive delivery:** an event emitted by the server should reach
//!    the consumer BEFORE the server has closed the SSE body. With the old
//!    `Vec<WireEvent>`-materializing path, the consumer got nothing until
//!    the server closed — operator-visible symptom: "Copilot just stops
//!    sometimes" while the stream is actually mid-flight.
//!
//! 2. **Stall-surfacing:** when the server emits N events then stops sending
//!    (TCP stays half-open), the consumer should see an error within a
//!    bounded deadline. The HTTP-level `read_timeout` on the adapter's
//!    reqwest client is the safety net.
//!
//! Both tests use a hand-rolled gated HTTP server (see `common.rs`) because
//! wiremock writes the full body before closing the socket.

#[path = "common.rs"]
mod common;

use codex_api::ResponseEvent;
use codex_copilot::CopilotAuth;
use codex_copilot::CopilotConfig;
use codex_copilot_adapter::stream_inner;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use futures::StreamExt;
use std::time::Duration;
use std::time::Instant;

use common::GatedChunk;
use common::GatedResponseSpec;
use common::done_sentinel;
use common::jwt_envelope;
use common::sse_line;
use common::start_gated_server_with;

fn user_msg(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".into(),
        content: vec![ContentItem::InputText { text: text.into() }],
        phase: None,
    }
}

fn content_chunk(text: &str) -> String {
    format!(r#"{{"choices":[{{"delta":{{"content":"{text}"}}}}]}}"#)
}

const STOP_CHUNK: &str = r#"{"choices":[{"finish_reason":"stop","delta":{}}]}"#;

fn config_from(uri: &str) -> CopilotConfig {
    CopilotConfig {
        github_api_base: uri.to_string(),
        github_oauth_base: uri.to_string(),
        copilot_api_base: uri.to_string(),
        force_allow_headless: true,
        device_poll_interval: Some(Duration::from_millis(10)),
        device_poll_max: Some(Duration::from_secs(30)),
    }
}

/// The consumer should observe each content delta as its SSE line arrives on
/// the wire, not after the entire response body has been buffered.
///
/// We gate the second content chunk so the server cannot write it until the
/// test releases the gate. If the adapter is truly streaming, the consumer
/// will receive the FIRST delta before we release the SECOND chunk's gate.
/// If the adapter materializes, the consumer blocks on `stream.next().await`
/// until we release every gate, proving the bug.
#[tokio::test]
async fn stream_inner_delivers_events_progressively() {
    let (gate2_tx, chunk2) = GatedChunk::gated(sse_line(&content_chunk(" world")));
    // Stuff the gate into an Option so we can take ownership across the
    // builder closure boundary.
    let mut chunk2_opt = Some(chunk2);
    let server = start_gated_server_with(|uri| GatedResponseSpec {
        jwt_body: jwt_envelope(uri, 1500),
        chat_chunks: vec![
            GatedChunk::immediate(sse_line(&content_chunk("hello"))),
            chunk2_opt.take().expect("chunk2 used once"),
            GatedChunk::immediate(sse_line(STOP_CHUNK)),
            GatedChunk::immediate(done_sentinel()),
        ],
        stall_after_chunks: false,
    })
    .await;

    let http = reqwest::Client::builder()
        .read_timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let cfg = config_from(server.uri());
    let mut auth = CopilotAuth::from_token(http.clone(), "ghu_progressive_test".into(), cfg);

    let input = vec![user_msg("say hi")];
    let mut stream = stream_inner(&http, &mut auth, &input, "gpt-4o", &[])
        .await
        .expect("stream_inner");

    // Pull events until we see "hello". This MUST complete before we release
    // gate2 — otherwise the adapter is materializing.
    let deadline = Duration::from_secs(3);
    let first_delta = tokio::time::timeout(deadline, async {
        loop {
            match stream.next().await {
                Some(Ok(ResponseEvent::OutputTextDelta(s))) => return Some(s),
                Some(Ok(_)) => continue,
                Some(Err(e)) => panic!("adapter error before first delta: {e:?}"),
                None => return None,
            }
        }
    })
    .await
    .expect("timeout waiting for first delta — adapter is not streaming progressively")
    .expect("stream closed before first delta");

    assert_eq!(first_delta, "hello");

    // Now release the gate and drain the rest.
    let _ = gate2_tx.send(());

    let mut deltas = vec![first_delta];
    let mut stop_reason = None;
    while let Some(ev) = stream.next().await {
        match ev.expect("no adapter error") {
            ResponseEvent::OutputTextDelta(s) => deltas.push(s),
            ResponseEvent::Completed {
                stop_reason: sr, ..
            } => stop_reason = sr,
            _ => {}
        }
    }
    assert_eq!(deltas, vec!["hello".to_string(), " world".to_string()]);
    assert_eq!(stop_reason.as_deref(), Some("end_turn"));

    server.shutdown().await;
}

/// If the server emits a content delta then stalls (no more bytes, socket
/// stays half-open), the consumer should receive an error within a bounded
/// deadline — not hang forever.
///
/// This exercises the HTTP-layer `read_timeout` we stamped on the adapter's
/// reqwest client. With progressive streaming, the consumer may also see
/// the partial delta before the error; the hard requirement is that the
/// stream terminates under the timeout instead of black-holing.
#[tokio::test]
async fn stream_inner_surfaces_stall_as_error() {
    let server = start_gated_server_with(|uri| GatedResponseSpec {
        jwt_body: jwt_envelope(uri, 1500),
        chat_chunks: vec![GatedChunk::immediate(sse_line(&content_chunk("partial")))],
        // Hold the socket after the partial — simulates upstream stall.
        stall_after_chunks: true,
    })
    .await;

    let http = reqwest::Client::builder()
        .read_timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let cfg = config_from(server.uri());
    let mut auth = CopilotAuth::from_token(http.clone(), "ghu_stall_test".into(), cfg);

    let input = vec![user_msg("hi")];
    let started = Instant::now();
    let mut stream = stream_inner(&http, &mut auth, &input, "gpt-4o", &[])
        .await
        .expect("stream_inner");

    // Hard deadline of 10s to prove the client doesn't hang forever.
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        let mut got_delta = false;
        let mut got_error = false;
        while let Some(ev) = stream.next().await {
            match ev {
                Ok(ResponseEvent::OutputTextDelta(s)) if s == "partial" => got_delta = true,
                Ok(ResponseEvent::OutputTextDelta(_)) => {}
                Ok(ResponseEvent::Completed { .. }) => {
                    panic!("unexpected Completed on stalled stream");
                }
                Ok(_) => continue,
                Err(_) => {
                    got_error = true;
                    break;
                }
            }
        }
        (got_delta, got_error)
    })
    .await
    .expect("consumer hung waiting for stalled stream — read_timeout did not fire");

    let elapsed = started.elapsed();
    assert!(
        outcome.1,
        "stalled stream should surface an error; got_delta={}, got_error={}",
        outcome.0, outcome.1
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "stall took too long to surface: {elapsed:?}"
    );

    server.shutdown().await;
}

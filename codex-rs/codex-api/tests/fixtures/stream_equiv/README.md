# stream_equiv fixtures

This directory holds offline goldens for `tests/stream_equiv.rs`.

Each fixture lives in `eq-XX-name/` and should include:

- `metadata.json` — fixture id, provenance, spokes, and fidelity mode
- `messages_sse.json` — exact SSE lines as a JSON string array, including blank separators

Choose one assertion mode per fixture:

- **stream ≡ non-stream**: add `non_stream_message.json`
- **cross-wire /responses**: additionally add `responses_events.json` and `non_stream_response.json`
- **policy-only**: add `policy_assertions.json` (or `policy_assertions.xli.json` for an XLI-only override)

Notes:

- `stream_equiv.rs` prefers `policy_assertions.xli.json` over `policy_assertions.json` when both exist.
- Fixtures whose `metadata.json` omits `xli` from `spokes` are skipped by this test file even if they are registered.
- Some legacy fixtures also carry `expected_response_items.json` as a reference artifact; `stream_equiv.rs` does not read it.

When adding a fixture:

1. Pick the next `eq-XX` directory name.
2. Keep the SSE payload byte-stable in `messages_sse.json`.
3. Register the fixture in `tests/stream_equiv.rs`.
4. Refresh `MANIFEST.sha256` from this directory root, excluding the manifest itself.

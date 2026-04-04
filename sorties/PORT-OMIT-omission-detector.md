# PORT-OMIT — Omission Placeholder Detector

**Priority:** 🟠 P1 — Cross-Pollination
**Complexity:** Small (1-2 hours)
**Source:** Apex FQ-14
**Target:** `codex-rs/core/src/tools/handlers/` (edit/write tool handlers)
**Upstream Risk:** LOW — small check added to tool output validation

## Problem

When Claude writes or edits files, it sometimes outputs omission placeholders:
- `// ... existing code ...`
- `// ... rest of file ...`
- `/* ... remaining implementation ... */`
- `# ... (other methods remain unchanged) ...`

These silently truncate code. The file is written with the placeholder text literally included, replacing actual code.

## Implementation

Add a `detect_omission_placeholders()` function that checks tool output before file writes:

```rust
/// Detect common omission placeholder patterns in code output.
/// Returns true if the content contains patterns indicating the model
/// truncated its output with a placeholder comment.
fn detect_omission_placeholders(content: &str) -> bool {
    let patterns = [
        r"//\s*\.\.\.\s*(existing|rest|remaining|other)",
        r"/\*\s*\.\.\.\s*(existing|rest|remaining|other)",
        r"#\s*\.\.\.\s*(existing|rest|remaining|other)",
        r"<!--\s*\.\.\.\s*(existing|rest|remaining|other)",
        r"//\s*\.\.\.\s*\(",                              // // ... (methods)
        r"//\s*(rest of|existing|remaining)\s+(the\s+)?(file|code|implementation)",
    ];
    
    patterns.iter().any(|pat| {
        regex::Regex::new(pat).unwrap().is_match(content)
    })
}
```

When detected:
1. Log a warning
2. Reject the file write with an error message to the model: "Your output contains omission placeholders. Please provide the complete file content."
3. The model will retry with full content

### Integration Point

In the write_file / str_replace tool handlers, check the output content before applying the write. This is after the model produces output but before the file is modified.

## Upstream Compatibility

LOW risk. If integrated into upstream tool handlers, the change is a small validation check. If we add it as a wrapper/hook, it's even safer.

## Tests

| # | Test | Expected |
|---|------|----------|
| 1 | `// ... existing code ...` → detected | True |
| 2 | `/* ... rest of implementation ... */` → detected | True |
| 3 | `# ... remaining methods ...` → detected | True |
| 4 | Normal code with `...` in strings → NOT detected | False (string content ok) |
| 5 | Ellipsis in comments that aren't omissions → NOT detected | False |
| 6 | Empty content → NOT detected | False |

## Build & Verify

```bash
cd codex-rs
cargo check -p codex-core
cargo test -p codex-core -- omission
```

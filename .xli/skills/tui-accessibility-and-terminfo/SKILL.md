---
name: tui-accessibility-and-terminfo
description: Use when working on color rendering, emoji display, character width calculations, or terminal capability detection. Owns the terminal-detection crate usage, truecolor vs 256 vs mono fallback, NO_COLOR spec compliance, Windows ConPTY quirks, and narrow/wide grapheme handling.
allowed-tools: Read, Edit, Grep, Bash(cargo nextest run -p codex-tui *), Bash(cargo nextest run -p terminal-detection *)
---

# TUI accessibility and terminfo

## When to load
Any diff touching color output, emoji rendering, character width calculations,
or terminal capability detection.

## Key concepts
- **terminal-detection crate**: In-tree crate at `codex-rs/terminal-detection/`.
  Use this for capability detection — do not roll your own `$TERM` parsing.
- **Color fallback chain**: truecolor (`COLORTERM=truecolor`) → 256-color →
  16-color → mono. Always provide a mono fallback for every styled element.
- **NO_COLOR**: Respect the `NO_COLOR` environment variable
  (https://no-color.org/). When set, suppress ALL color output. This is
  non-negotiable accessibility compliance.
- **Unicode width**: Use `unicode-width::UnicodeWidthStr` for display width.
  CJK characters are width-2. Emoji with ZWJ sequences may be width-2 but
  render as width-1 in some terminals — test with real terminals.
- **Windows ConPTY**: Windows Terminal supports most ANSI sequences via ConPTY,
  but legacy `cmd.exe` does not. The `terminal-detection` crate handles this
  distinction.

## Workflow
1. Identify the terminal capability assumption being made.
2. Verify the `terminal-detection` crate handles it.
3. Test with `NO_COLOR=1` to confirm graceful degradation.
4. If adding emoji or special characters, verify width calculations.
5. `cargo nextest run -p codex-tui -p terminal-detection` green.

## Anti-patterns
- Hardcoding ANSI color codes without checking terminal capabilities.
- Ignoring `NO_COLOR`.
- Using `char::len_utf8()` or `str::len()` for display width.
- Assuming all terminals support truecolor.

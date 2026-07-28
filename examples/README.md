# xli — Anthropic & Gemini Quick Start

Example configurations for running `xli` directly against the public
first-party APIs: Anthropic's Messages API and Google's Gemini
`generateContent` API.

| File | Use it for |
|---|---|
| [`anthropic.toml`](anthropic.toml) | Claude models via `api.anthropic.com` |
| [`gemini.toml`](gemini.toml) | Gemini models via `generativelanguage.googleapis.com` |
| [`multi-provider.toml`](multi-provider.toml) | Both providers in one config, switchable with `-p` |

---

## 1. Config file location

`xli` reads a single TOML file:

```
~/.xli/config.toml
```

Override the directory with `XLI_HOME` (`CODEX_HOME` is honoured as a
legacy fallback). If `XLI_HOME` is set it must already exist and be a
directory — `xli` will not create it for you.

```bash
export XLI_HOME="$HOME/.config/xli"    # optional
```

Install one of the examples:

```bash
mkdir -p ~/.xli
cp multi-provider.toml ~/.xli/config.toml
```

---

## 2. Environment variables

Set the key(s) for the provider(s) you configured. The config files
reference these by name — no secret is ever written to disk.

```bash
# Anthropic
export ANTHROPIC_API_KEY="your-api-key-here"

# Google Gemini
export GEMINI_API_KEY="your-api-key-here"
```

Get keys from <https://console.anthropic.com/> and
<https://aistudio.google.com/apikey>.

---

## 3. Launching

```bash
xli                            # top-level model + model_provider defaults
xli -p sonnet                  # named profile from [profiles.*]
xli -p gemini-flash
xli -m claude-sonnet-4-6       # ad-hoc model, keeps the current provider
xli -c model_reasoning_effort=xhigh   # one-off config override
```

`-m/--model` only swaps the model slug; it does **not** change the
provider. To cross a provider boundary use a profile, or pass both:

```bash
xli -m gemini-2.5-pro -c model_provider=gemini
```

### Profiles in these examples

| Profile | Model | Provider |
|---|---|---|
| `opus` | `claude-opus-4-7` | anthropic |
| `sonnet` | `claude-sonnet-4-6` | anthropic |
| `haiku` | `claude-haiku-4-5-20251001` | anthropic |
| `gemini-pro` | `gemini-3.1-pro-preview` | gemini |
| `gemini-flash` | `gemini-3-flash-preview` | gemini |
| `gemini-lite` | `gemini-3.1-flash-lite-preview` | gemini |
| `gemini-25-pro` | `gemini-2.5-pro` | gemini |
| `gemini-25-flash` | `gemini-2.5-flash` | gemini |

Any model slug the API accepts will work — the catalog falls back to
family heuristics for unrecognised names, so newer releases do not
require an `xli` upgrade.

---

## 4. Why `env_http_headers` and not `env_key`

This is the single most important detail, and the reason you cannot
just write `env_key = "ANTHROPIC_API_KEY"`.

`env_key` resolves the named environment variable and sends the value as:

```
Authorization: Bearer <key>
```

That is the OpenAI convention. Anthropic requires `x-api-key` and Gemini
requires `x-goog-api-key`. Configured with `env_key` alone, both APIs
return **401**.

`env_http_headers` maps *header name → environment variable name*, and
those headers are attached to every outgoing request:

```toml
env_http_headers = { "x-api-key"      = "ANTHROPIC_API_KEY" }
env_http_headers = { "x-goog-api-key" = "GEMINI_API_KEY" }
```

Do not set both `env_key` and `env_http_headers` for the same provider —
you would send two competing auth headers.

The `?key=<api-key>` query-string style that Gemini also supports is
**not** usable here: the request path already ends in `?alt=sse`, and
`query_params` are appended with a second `?`, producing a malformed URL.

---

## 5. Provider block reference

Only the fields used by these examples; all are optional unless noted.

| Field | Meaning |
|---|---|
| `name` | Display name shown in the TUI |
| `base_url` | API root. `messages` / `models/…:streamGenerateContent` is appended |
| `wire_api` | `messages` (Anthropic), `generate_content` (Gemini), `responses`, `copilot` |
| `requires_openai_auth` | Set `false` for third-party providers so `xli` does not demand an OpenAI login |
| `env_key` | Env var whose value is sent as `Authorization: Bearer` |
| `env_http_headers` | Map of header name → env var name |
| `http_headers` | Map of header name → literal value |
| `query_params` | Map appended to the URL query string (avoid on the Gemini wire) |
| `request_max_retries`, `stream_max_retries`, `stream_idle_timeout_ms` | Transport tuning |

Correct base URLs:

```
https://api.anthropic.com/v1                        # wire_api = "messages"
https://generativelanguage.googleapis.com/v1beta    # wire_api = "generate_content"
```

There is no built-in `anthropic` or `gemini` provider — the
`[model_providers.*]` block is required. Without it, an unset `base_url`
falls back to `https://api.openai.com/v1`.

---

## 6. Telemetry

Release builds default `metrics_exporter` to `"statsig"`, which posts
usage metrics to a third-party endpoint. All three examples disable it:

```toml
[otel]
metrics_exporter = "none"   # none | statsig | otlp-http
log_user_prompt = false
```

---

## 7. Known limitations

Behaviours worth knowing before you hit them:

- **No missing-key diagnostics.** Because `env_key` is deliberately
  omitted, `xli` does not pre-flight validate that your key variable is
  set. Forget to `export ANTHROPIC_API_KEY` and you get a raw upstream
  401 rather than a helpful message. Verify with
  `echo "${ANTHROPIC_API_KEY:?not set}"` before a long session.

- **Stale credentials in `auth.json` can leak.** With `env_key` omitted,
  auth resolution falls through to the shared auth manager. If
  `$XLI_HOME/auth.json` holds an OpenAI API key from a previous setup,
  an `Authorization: Bearer <that-key>` header may be attached to
  requests bound for `api.anthropic.com` or
  `generativelanguage.googleapis.com`. Run `xli logout`, or remove
  `auth.json`, before using these configs. (ChatGPT-login tokens are
  filtered out and are not affected.)

- **Gemini 3 output is capped at 8,192 tokens.** The output-token
  ceiling resolves to 65,536 only for `gemini-2.5*` slugs; every Gemini 3
  slug silently gets 8,192. Use a `gemini-2.5-*` profile if you need long
  single responses.

- **Anthropic requests are always streaming.** `stream: true` is
  hardcoded on the Messages wire. Fine for interactive use; there is no
  non-streaming/batch path.

- **Reasoning effort accepts** `none`, `minimal`, `low`, `medium`
  (default), `high`, `xhigh`. On Gemini this maps to
  `thinkingConfig.thinkingBudget`; `none` disables thinking.

---

## 8. Troubleshooting

| Symptom | Cause |
|---|---|
| `401` from `api.anthropic.com` | Used `env_key` instead of `env_http_headers` — see §4 |
| `401`/`403` from Gemini with no `xli` error | Key env var unset; `xli` fell back to unauthenticated |
| Requests hitting `api.openai.com` | `base_url` unset, or `model_provider` does not match your `[model_providers.*]` key |
| Prompted for an OpenAI login | `requires_openai_auth` not set to `false` |
| Malformed URL containing `?alt=sse?key=` | Remove `query_params`; use `env_http_headers` |
| `XLI_HOME points to … does not exist` | Create the directory first |

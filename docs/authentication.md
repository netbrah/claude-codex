# Authentication

The CLI supports both managed OpenAI/ChatGPT auth and provider-native auth.
Which path you use depends on the model provider configured for the session.

## Auth modes at a glance

| Mode | Best for | How it is configured |
|---|---|---|
| Managed OpenAI / ChatGPT auth | Upstream Codex/OpenAI flows | `xli login` / `codex login` |
| API key on stdin | Non-interactive OpenAI setup | `xli login --with-api-key` |
| Access token on stdin | Advanced managed-token workflows | `xli login --with-access-token` |
| Provider-native headers | Anthropic, Gemini, or custom providers | `config.toml` + environment variables |

## Managed login flow

Start a login interactively:

```bash
xli login
```

Useful variants:

```bash
xli login status
xli login --device-auth
printenv OPENAI_API_KEY | xli login --with-api-key
printenv CODEX_ACCESS_TOKEN | xli login --with-access-token
xli logout
```

Managed auth is appropriate when you are using the standard OpenAI/Codex auth
path. Credentials are stored under the active Codex home directory, which in
this fork is usually `~/.xli`.

## Provider-native auth via config

For Anthropic, Gemini, and other non-OpenAI providers, the usual pattern is to
keep secrets in environment variables and reference them from `config.toml`.

Anthropic example:

```toml
[model_providers.anthropic]
name = "Anthropic"
base_url = "https://api.anthropic.com/v1"
wire_api = "messages"
requires_openai_auth = false
env_http_headers = { "x-api-key" = "ANTHROPIC_API_KEY" }
```

Gemini example:

```toml
[model_providers.gemini]
name = "Google Gemini"
base_url = "https://generativelanguage.googleapis.com/v1beta"
wire_api = "generate_content"
requires_openai_auth = false
env_http_headers = { "x-goog-api-key" = "GEMINI_API_KEY" }
```

Export the corresponding variables before you launch the CLI:

```bash
export ANTHROPIC_API_KEY="your-key"
export GEMINI_API_KEY="your-key"
```

## `env_http_headers` vs `env_key`

Use `env_http_headers` when the upstream API expects a vendor-specific header.

- `env_key` sends the standard bearer authorization header
- `env_http_headers` sends the header name you specify

That distinction matters because Anthropic expects `x-api-key` and Gemini
expects `x-goog-api-key`.

## Choosing the right auth strategy

Use managed login when:

- you are using the default OpenAI/Codex provider path
- you want the CLI to own token refresh and account state

Use provider-native env vars when:

- you are targeting Anthropic `/messages`
- you are targeting Gemini `generateContent`
- you are using a custom or enterprise provider definition in `model_providers`

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| Prompted to log in even though you set a provider block | `requires_openai_auth = false` is missing from the provider config |
| `401` from Anthropic or Gemini | You used `env_key` instead of `env_http_headers`, or the env var is unset |
| Requests go to OpenAI unexpectedly | `model_provider` does not match a configured `[model_providers.<name>]` entry |
| Login works in one environment but not another | `CODEX_HOME`/`XLI_HOME` points at a different state directory |

For working provider-native examples, see [example-config.md](example-config.md)
and [`../examples/README.md`](../examples/README.md).

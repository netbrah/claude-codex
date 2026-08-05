# Sample configuration

The repository ships ready-to-copy examples under [`../examples/`](../examples/).
Start there before you build a config from scratch.

## Included examples

| File | Best for |
|---|---|
| [`../examples/anthropic.toml`](../examples/anthropic.toml) | Single-provider Claude setup |
| [`../examples/gemini.toml`](../examples/gemini.toml) | Single-provider Gemini setup |
| [`../examples/multi-provider.toml`](../examples/multi-provider.toml) | One config with multiple providers and profiles |

## Minimal Anthropic example

```toml
model = "claude-sonnet-4-6"
model_provider = "anthropic"
model_reasoning_effort = "medium"

[model_providers.anthropic]
name = "Anthropic"
base_url = "https://api.anthropic.com/v1"
wire_api = "messages"
requires_openai_auth = false
env_http_headers = { "x-api-key" = "ANTHROPIC_API_KEY" }
```

## Multi-provider pattern

Use profiles when you want a single config to switch across vendors:

```toml
model = "claude-opus-4-7"
model_provider = "anthropic"

[model_providers.anthropic]
name = "Anthropic"
base_url = "https://api.anthropic.com/v1"
wire_api = "messages"
requires_openai_auth = false
env_http_headers = { "x-api-key" = "ANTHROPIC_API_KEY" }

[model_providers.gemini]
name = "Google Gemini"
base_url = "https://generativelanguage.googleapis.com/v1beta"
wire_api = "generate_content"
requires_openai_auth = false
env_http_headers = { "x-goog-api-key" = "GEMINI_API_KEY" }

[profiles.opus]
model = "claude-opus-4-7"
model_provider = "anthropic"

[profiles.gemini-pro]
model = "gemini-3.1-pro-preview"
model_provider = "gemini"
```

## Recommended workflow

1. Create the active home directory:

   ```bash
   mkdir -p ~/.xli
   ```

2. Copy the example you want:

   ```bash
   cp examples/multi-provider.toml ~/.xli/config.toml
   ```

3. Export the environment variables referenced by the file.
4. Launch `xli` or run `xli -p <profile>`.

## Common adjustments

The shipped examples also demonstrate a few high-value settings:

- `model_reasoning_effort`
- `model_reasoning_summary`
- `web_search`
- `[profiles.*]`
- `[otel] metrics_exporter = "none"`
- `[tui] theme = "nord"`

## Common mistakes

| Mistake | Fix |
|---|---|
| Using `env_key` for Anthropic or Gemini | Use `env_http_headers` instead |
| Switching providers with `-m` alone | Use profiles, or set both `model` and `model_provider` |
| Forgetting `requires_openai_auth = false` | Add it for provider-native endpoints |
| Writing secrets into `config.toml` | Keep them in environment variables |

See [config.md](config.md) for the full layering model and
[authentication.md](authentication.md) for auth-specific guidance.

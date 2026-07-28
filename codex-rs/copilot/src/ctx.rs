//! `CopilotCtx` — session-scoped Copilot auth + endpoint snapshot.
//!
//! See OPROD 04 §5 SEAM-F / §6 PHASE 1. The upstream
//! `codex_copilot::CopilotAuth` already owns ghu_ → CAPI token exchange,
//! expiry caching, and endpoint resolution. This wrapper provides:
//!
//! 1. A `tokio::sync::Mutex`-guarded handle so concurrent turns don't race
//!    token mint.
//! 2. An atomic `bearer()` call that returns a fresh CAPI bearer **and** the
//!    live `endpoints.api` URL together — so token refresh never leaves us
//!    with a bearer pointing at a stale base URL (rubber-duck finding #1).
//! 3. A `force_refresh()` path for the 401-retry branches in
//!    `core/src/client.rs` (rubber-duck finding #1).
//!
//! Design notes per rubber-duck consult:
//! - No new `codex_api::AuthProvider` impl. The sync trait is satisfied by
//!   stuffing a pre-minted bearer into `CoreAuthProvider`.
//! - Ctx is intentionally `!Clone`; callers hold `Arc<CopilotCtx>`.

use std::sync::Arc;

use codex_copilot::CopilotAuth;
use codex_copilot::CopilotAuthError;
use tokio::sync::Mutex;

/// Atomic snapshot of the currently-valid Copilot auth state.
///
/// `Debug` is hand-rolled to redact the bearer — this struct is returned
/// from `CopilotCtx::snapshot` / `force_refresh` and is public, so anything
/// that formats it with `{:?}` (panic, `tracing::debug!`, test assertion)
/// must not leak the CAPI token. See OPROD 04 §7 OPSEC rule 1.
#[derive(Clone)]
pub struct CopilotAuthSnapshot {
    /// CAPI bearer JWT. Field is private so the only public way to read
    /// it is `bearer()` — that gives us a single chokepoint to add OPSEC
    /// instrumentation (rate-limit, audit, redaction) if it ever becomes
    /// necessary, and prevents accidental moves into containers whose
    /// `Debug` impl we do not control. F2 / OPSEC rule 1.
    bearer: String,
    /// Live endpoint base URL (`api.enterprise.githubcopilot.com` or
    /// `api.githubcopilot.com`). Pulled from the CAPI envelope on every
    /// token mint — never hardcoded (OPROD §7 rule 2).
    pub endpoints_api: String,
}

impl CopilotAuthSnapshot {
    /// Construct a snapshot from raw mint outputs. Crate-private so callers
    /// outside the adapter cannot fabricate a snapshot bypassing
    /// `CopilotCtx::snapshot` / `force_refresh`.
    pub(crate) fn new(bearer: String, endpoints_api: String) -> Self {
        Self {
            bearer,
            endpoints_api,
        }
    }

    /// Borrow the CAPI bearer JWT. Callers MUST pass this directly to the
    /// outbound `Authorization` header and not retain a copy. The bearer
    /// rotates on every `force_refresh` and stale copies will get 401s.
    #[must_use]
    pub fn bearer(&self) -> &str {
        &self.bearer
    }

    /// Move the bearer out of the snapshot when the caller needs an owned
    /// `String` for header construction. Consumes the snapshot to make the
    /// move explicit at the call site (no accidental clones).
    #[must_use]
    pub fn into_bearer(self) -> String {
        self.bearer
    }
}

impl std::fmt::Debug for CopilotAuthSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopilotAuthSnapshot")
            .field("bearer", &"<redacted>")
            .field("endpoints_api", &self.endpoints_api)
            .finish()
    }
}

/// Session-scoped Copilot context. One per `ModelClient` session.
#[derive(Debug)]
pub struct CopilotCtx {
    /// Upstream auth handle owns the cache, ghu_ discovery, and mint.
    /// Wrapped in a `tokio::sync::Mutex` because upstream's `token()` takes
    /// `&mut self`.
    auth: Mutex<CopilotAuth>,
    /// HTTP client retained for CAPI `/models` fetches. Same client used
    /// by `CopilotAuth` for the JWT exchange — reusing it keeps the
    /// connection pool warm and respects whatever cert/proxy config the
    /// host environment installed.
    http: reqwest::Client,
    /// Per-session CAPI catalog. Lazy-fetched on first
    /// [`Self::models`] call; cached for the rest of the session. On
    /// fetch failure the cell holds an empty catalog so we don't keep
    /// retrying — provider fallback paths take over.
    models: tokio::sync::OnceCell<crate::capi_models::CapiModelCatalog>,
    /// Shared headers stamped on every CAPI request — `editor-version`,
    /// `copilot-integration-id`, etc. Stored once at construction so the
    /// `/models` fetcher can reuse them without re-deriving.
    extra_headers: http::HeaderMap,
}

impl CopilotCtx {
    /// Initialize from the host environment — reads ghu_ from disk (VS Code
    /// OAuth config, `~/.config/github-copilot/hosts.json`, or device flow
    /// stub). Does NOT mint a CAPI bearer yet; that happens on first
    /// [`Self::snapshot`] call.
    pub async fn init(http: reqwest::Client) -> Result<Self, CopilotAuthError> {
        let auth = CopilotAuth::init(http.clone()).await?;
        Ok(Self {
            auth: Mutex::new(auth),
            http,
            models: tokio::sync::OnceCell::new(),
            extra_headers: http::HeaderMap::new(),
        })
    }

    /// Construct with explicit shared headers for CAPI requests. The
    /// caller supplies the four wire-agnostic Copilot headers that
    /// every CAPI call needs (`editor-version`,
    /// `copilot-integration-id`, `x-initiator`, `x-github-api-version`).
    /// `init` defaults to an empty header map; callers in `codex-core`
    /// that already build the headers via
    /// `codex_provider_copilot::stamp_copilot_shared_headers` should
    /// prefer this constructor so the `/models` fetch carries the same
    /// envelope as inference traffic.
    pub async fn init_with_headers(
        http: reqwest::Client,
        extra_headers: http::HeaderMap,
    ) -> Result<Self, CopilotAuthError> {
        let auth = CopilotAuth::init(http.clone()).await?;
        Ok(Self {
            auth: Mutex::new(auth),
            http,
            models: tokio::sync::OnceCell::new(),
            extra_headers,
        })
    }

    /// Build from an already-configured `CopilotAuth` (test / DI hook).
    #[must_use]
    pub fn from_auth(auth: CopilotAuth) -> Self {
        Self {
            auth: Mutex::new(auth),
            http: reqwest::Client::new(),
            models: tokio::sync::OnceCell::new(),
            extra_headers: http::HeaderMap::new(),
        }
    }

    /// Return a fresh auth snapshot: valid bearer + live `endpoints.api`.
    ///
    /// Mints lazily on first call; reuses cached bearer if still valid. The
    /// `endpoints_api` is always read from the same cache entry as the
    /// bearer so the two can never disagree.
    pub async fn snapshot(&self) -> Result<CopilotAuthSnapshot, CopilotAuthError> {
        let mut guard = self.auth.lock().await;
        let bearer = guard.token().await?;
        let endpoints_api = guard
            .endpoints()
            .map(|e| e.api.clone())
            .ok_or(CopilotAuthError::MalformedResponse("endpoints.api"))?;
        Ok(CopilotAuthSnapshot::new(bearer, endpoints_api))
    }

    /// Invalidate the cached bearer and mint a new one. Called from the
    /// 401-retry branches in `core/src/client.rs` when Copilot returns
    /// Unauthorized on a Messages/Responses request.
    pub async fn force_refresh(&self) -> Result<CopilotAuthSnapshot, CopilotAuthError> {
        let mut guard = self.auth.lock().await;
        guard.cache_mut().force_refresh().await?;
        let bearer = guard.token().await?;
        let endpoints_api = guard
            .endpoints()
            .map(|e| e.api.clone())
            .ok_or(CopilotAuthError::MalformedResponse("endpoints.api"))?;
        Ok(CopilotAuthSnapshot::new(bearer, endpoints_api))
    }

    /// Returns the CAPI `/models` catalog for this session, fetching it
    /// on first call and caching the result.
    ///
    /// Failure modes (network, auth, parse) cache an empty catalog so
    /// subsequent calls don't re-attempt the fetch — provider fallback
    /// paths (family-default heuristics) take over and the session keeps
    /// running. The TUI display will continue to show the inflated
    /// family-default context window in that case, but inference still
    /// works because compaction runs against the model's
    /// `max_prompt_tokens` regardless of what the display shows.
    ///
    /// Concurrency: safe to call from multiple tasks; the inner
    /// `OnceCell` ensures the fetch runs at most once.
    pub async fn models(&self) -> crate::capi_models::CapiModelCatalog {
        self.models
            .get_or_init(|| async {
                let snapshot = match self.snapshot().await {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::warn!(
                            target: "copilot::ctx",
                            error = %err,
                            "models(): snapshot failed; caching empty catalog"
                        );
                        return std::sync::Arc::new(std::collections::HashMap::new());
                    }
                };
                crate::capi_models::fetch_capi_models(
                    &self.http,
                    snapshot.bearer(),
                    &snapshot.endpoints_api,
                    &self.extra_headers,
                )
                .await
            })
            .await
            .clone()
    }
}

/// Convenience: shareable handle. Most callers will hold
/// `Arc<CopilotCtx>`.
pub type SharedCopilotCtx = Arc<CopilotCtx>;

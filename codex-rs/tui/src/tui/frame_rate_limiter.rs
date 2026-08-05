//! Limits how frequently frame draw notifications may be emitted.
//!
//! Widgets sometimes call `FrameRequester::schedule_frame()` more frequently than a user can
//! perceive. This limiter clamps draw notifications to a maximum frame rate to avoid wasted work.
//!
//! XLI addition: the ceiling is adaptive. On a local terminal the upstream 120 FPS default is
//! kept, but when rendering over a high-latency link (SSH) the default drops to 60 FPS so frames
//! cannot queue faster than the terminal can drain them. The ceiling can also be pinned explicitly
//! with the `XLI_MAX_FPS` environment variable. See `~/Projects/xli-ops/findings` (R3).
//!
//! This is intentionally a small, pure helper so it can be unit-tested in isolation and used by
//! the async frame scheduler without adding complexity to the app/event loop.

use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

/// A 120 FPS minimum frame interval (≈8.33ms). The upstream default and local ceiling.
pub(super) const MIN_FRAME_INTERVAL: Duration = Duration::from_nanos(8_333_334);

/// A 60 FPS minimum frame interval (≈16.67ms). The default ceiling over a remote terminal.
const REMOTE_FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

/// Clamp bounds for an explicit `XLI_MAX_FPS` override.
const MIN_ALLOWED_FPS: u32 = 1;
const MAX_ALLOWED_FPS: u32 = 240;

/// Resolve the minimum frame interval from an optional `XLI_MAX_FPS` value and remote-ness.
///
/// Precedence: an explicit, parseable `XLI_MAX_FPS` wins (clamped to a sane range); otherwise a
/// remote terminal gets the 60 FPS ceiling and a local terminal keeps the 120 FPS default. This is
/// pure so it can be unit-tested without touching process state.
fn resolve_min_frame_interval(max_fps_env: Option<&str>, remote: bool) -> Duration {
    if let Some(fps) = max_fps_env
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<u32>().ok())
    {
        let fps = fps.clamp(MIN_ALLOWED_FPS, MAX_ALLOWED_FPS);
        return Duration::from_secs(1) / fps;
    }
    if remote {
        REMOTE_FRAME_INTERVAL
    } else {
        MIN_FRAME_INTERVAL
    }
}

/// Heuristic: is the TUI being rendered over a remote (high-latency) terminal?
///
/// True when an SSH session is detected, or when `TERM` is unset/empty/`dumb` (a common signal of
/// a non-interactive or proxied terminal).
fn is_remote_terminal() -> bool {
    if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
        return true;
    }
    match std::env::var("TERM") {
        Ok(term) => term.is_empty() || term == "dumb",
        Err(_) => true,
    }
}

/// The effective minimum frame interval for this process, resolved once.
pub(super) fn effective_min_frame_interval() -> Duration {
    static INTERVAL: OnceLock<Duration> = OnceLock::new();
    *INTERVAL.get_or_init(|| {
        let max_fps = std::env::var("XLI_MAX_FPS").ok();
        resolve_min_frame_interval(max_fps.as_deref(), is_remote_terminal())
    })
}

/// Remembers the most recent emitted draw, allowing deadlines to be clamped forward.
#[derive(Debug)]
pub(super) struct FrameRateLimiter {
    last_emitted_at: Option<Instant>,
    min_interval: Duration,
}

impl Default for FrameRateLimiter {
    fn default() -> Self {
        Self::new(MIN_FRAME_INTERVAL)
    }
}

impl FrameRateLimiter {
    /// Create a limiter that clamps draw notifications to at most one per `min_interval`.
    pub(super) fn new(min_interval: Duration) -> Self {
        Self {
            last_emitted_at: None,
            min_interval,
        }
    }

    /// Returns `requested`, clamped forward if it would exceed the maximum frame rate.
    pub(super) fn clamp_deadline(&self, requested: Instant) -> Instant {
        let Some(last_emitted_at) = self.last_emitted_at else {
            return requested;
        };
        let min_allowed = last_emitted_at
            .checked_add(self.min_interval)
            .unwrap_or(last_emitted_at);
        requested.max(min_allowed)
    }

    /// Records that a draw notification was emitted at `emitted_at`.
    pub(super) fn mark_emitted(&mut self, emitted_at: Instant) {
        self.last_emitted_at = Some(emitted_at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn default_does_not_clamp() {
        let t0 = Instant::now();
        let limiter = FrameRateLimiter::default();
        assert_eq!(limiter.clamp_deadline(t0), t0);
    }

    #[test]
    fn clamps_to_min_interval_since_last_emit() {
        let t0 = Instant::now();
        let mut limiter = FrameRateLimiter::default();

        assert_eq!(limiter.clamp_deadline(t0), t0);
        limiter.mark_emitted(t0);

        let too_soon = t0 + Duration::from_millis(1);
        assert_eq!(limiter.clamp_deadline(too_soon), t0 + MIN_FRAME_INTERVAL);
    }

    #[test]
    fn custom_interval_clamps_to_that_interval() {
        let t0 = Instant::now();
        let interval = Duration::from_millis(50);
        let mut limiter = FrameRateLimiter::new(interval);

        limiter.mark_emitted(t0);
        let too_soon = t0 + Duration::from_millis(1);
        assert_eq!(limiter.clamp_deadline(too_soon), t0 + interval);
    }

    #[test]
    fn resolve_local_defaults_to_120fps() {
        assert_eq!(
            resolve_min_frame_interval(None, /* remote */ false),
            MIN_FRAME_INTERVAL
        );
    }

    #[test]
    fn resolve_remote_defaults_to_60fps() {
        assert_eq!(
            resolve_min_frame_interval(None, /* remote */ true),
            REMOTE_FRAME_INTERVAL
        );
    }

    #[test]
    fn resolve_explicit_env_overrides_remote() {
        // An explicit cap wins even over the remote default.
        assert_eq!(
            resolve_min_frame_interval(Some("30"), /* remote */ true),
            Duration::from_secs(1) / 30
        );
    }

    #[test]
    fn resolve_clamps_out_of_range_env() {
        assert_eq!(
            resolve_min_frame_interval(Some("100000"), /* remote */ false),
            Duration::from_secs(1) / MAX_ALLOWED_FPS
        );
        assert_eq!(
            resolve_min_frame_interval(Some("0"), /* remote */ false),
            Duration::from_secs(1) / MIN_ALLOWED_FPS
        );
    }

    #[test]
    fn resolve_ignores_unparseable_env() {
        // Garbage falls through to the remote/local default.
        assert_eq!(
            resolve_min_frame_interval(Some("fast"), /* remote */ false),
            MIN_FRAME_INTERVAL
        );
        assert_eq!(
            resolve_min_frame_interval(Some(""), /* remote */ true),
            REMOTE_FRAME_INTERVAL
        );
    }
}

//! Loop detection for infinite tool-call and content-repetition loops.
//!
//! Monitors the sequence of tool calls and assistant content produced by the
//! model. When the most recent N entries are identical (by name + argument
//! hash for tools, or by content hash for assistant messages), the detector
//! signals that a loop has been detected so the caller can inject a
//! loop-breaking system message and pause the turn.

use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;

/// Default number of identical consecutive tool calls before a loop is detected.
const DEFAULT_TOOL_LOOP_THRESHOLD: usize = 5;

/// Default number of identical consecutive content hashes before a loop is detected.
const DEFAULT_CONTENT_LOOP_THRESHOLD: usize = 10;

/// Maximum history entries retained per ring buffer. Keeps memory bounded even
/// for very long sessions.
const MAX_HISTORY: usize = 64;

/// The message injected into the conversation when a loop is detected.
pub(crate) const LOOP_BREAK_MESSAGE: &str = "You appear to be in a loop, repeating the same tool calls or producing the same output \
     repeatedly. Please try a different approach.";

/// Tracks recent tool calls and assistant content to detect infinite loops.
#[derive(Debug)]
pub(crate) struct LoopDetector {
    /// Ring buffer of `(tool_name, args_hash)` for recent tool calls.
    tool_call_history: VecDeque<(String, u64)>,
    /// How many identical consecutive tool calls trigger detection.
    tool_loop_threshold: usize,

    /// Ring buffer of content hashes for recent assistant messages.
    content_hashes: VecDeque<u64>,
    /// How many identical consecutive content hashes trigger detection.
    content_loop_threshold: usize,
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopDetector {
    /// Create a new detector with default thresholds.
    pub(crate) fn new() -> Self {
        Self {
            tool_call_history: VecDeque::with_capacity(MAX_HISTORY),
            tool_loop_threshold: DEFAULT_TOOL_LOOP_THRESHOLD,
            content_hashes: VecDeque::with_capacity(MAX_HISTORY),
            content_loop_threshold: DEFAULT_CONTENT_LOOP_THRESHOLD,
        }
    }

    /// Record a tool call and return `true` if a tool loop is detected.
    pub(crate) fn record_tool_call(&mut self, name: &str, args: &str) -> bool {
        let hash = hash_string(args);
        if self.tool_call_history.len() >= MAX_HISTORY {
            self.tool_call_history.pop_front();
        }
        self.tool_call_history.push_back((name.to_string(), hash));
        self.detect_tool_loop()
    }

    /// Record an assistant content message and return `true` if a content loop
    /// is detected.
    pub(crate) fn record_content(&mut self, content: &str) -> bool {
        let hash = hash_string(content);
        if self.content_hashes.len() >= MAX_HISTORY {
            self.content_hashes.pop_front();
        }
        self.content_hashes.push_back(hash);
        self.detect_content_loop()
    }

    /// Reset all state. Called after a loop break so the detector starts fresh.
    pub(crate) fn reset(&mut self) {
        self.tool_call_history.clear();
        self.content_hashes.clear();
    }

    /// Check whether the last `tool_loop_threshold` entries in tool history
    /// are all identical (same tool name and same argument hash).
    fn detect_tool_loop(&self) -> bool {
        let threshold = self.tool_loop_threshold;
        if self.tool_call_history.len() < threshold {
            return false;
        }
        let last = match self.tool_call_history.back() {
            Some(entry) => entry,
            None => return false,
        };
        self.tool_call_history
            .iter()
            .rev()
            .take(threshold)
            .all(|entry| entry.0 == last.0 && entry.1 == last.1)
    }

    /// Check whether the last `content_loop_threshold` hashes are all
    /// identical.
    fn detect_content_loop(&self) -> bool {
        let threshold = self.content_loop_threshold;
        if self.content_hashes.len() < threshold {
            return false;
        }
        let last = match self.content_hashes.back() {
            Some(h) => *h,
            None => return false,
        };
        self.content_hashes
            .iter()
            .rev()
            .take(threshold)
            .all(|&h| h == last)
    }
}

/// Deterministic hash for a string slice. Uses `DefaultHasher` (SipHash)
/// for distribution quality; cryptographic strength is not required.
fn hash_string(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
#[path = "loop_detection_tests.rs"]
mod tests;

//! Tool-output masking service.
//!
//! When large tool outputs are replayed in subsequent API requests they can
//! exhaust smaller context windows (e.g. Sonnet sub-agents at 200K).  This
//! module replaces oversized outputs with a compact reference that preserves
//! the head and tail of the original text while persisting the full content
//! to disk so it can be recovered if needed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Default character-count threshold above which an output is eligible for
/// masking (~12.5K tokens at 4 chars/token).
pub const DEFAULT_THRESHOLD_CHARS: usize = 50_000;

/// Number of characters to keep from the head and tail of a masked output.
const PREVIEW_LEN: usize = 200;

/// Result of attempting to mask a tool output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaskResult {
    /// The output was below the threshold or the tool is exempt — pass
    /// through unchanged.
    Unmasked(String),
    /// The output was replaced with a compact reference.
    Masked {
        /// The compact replacement string to embed in the conversation.
        replacement: String,
        /// Path to the file on disk that holds the complete original output.
        original_path: PathBuf,
    },
}

/// Tool-output masking service.
///
/// See the module-level documentation for motivation.
#[derive(Debug)]
pub struct ToolOutputMasker {
    /// Directory where masked outputs are persisted.
    mask_dir: PathBuf,
    /// Character-count threshold: outputs shorter than this are never masked.
    threshold_chars: usize,
    /// Tool names that should never have their output masked.
    exempt_tools: HashSet<String>,
}

impl ToolOutputMasker {
    /// Create a new masker.
    ///
    /// `mask_dir` will be created on first write if it doesn't already exist.
    pub fn new(
        mask_dir: PathBuf,
        threshold_chars: usize,
        exempt_tools: HashSet<String>,
    ) -> Self {
        Self {
            mask_dir,
            threshold_chars,
            exempt_tools,
        }
    }

    /// Create a masker with default settings.
    ///
    /// Defaults:
    /// - `mask_dir`: `~/.xli/masked_outputs/`
    /// - `threshold_chars`: [`DEFAULT_THRESHOLD_CHARS`]
    /// - exempt tools: `ask_user_question`, `memory`
    pub fn with_defaults() -> Self {
        let mask_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".xli")
            .join("masked_outputs");

        let exempt_tools: HashSet<String> = [
            "ask_user_question".to_string(),
            "memory".to_string(),
        ]
        .into_iter()
        .collect();

        Self::new(mask_dir, DEFAULT_THRESHOLD_CHARS, exempt_tools)
    }

    /// Attempt to mask `output` produced by `tool_name`.
    ///
    /// Returns [`MaskResult::Unmasked`] when:
    /// - the tool is exempt, or
    /// - the output is shorter than `threshold_chars`.
    ///
    /// Returns [`MaskResult::Masked`] otherwise.  As a side-effect the full
    /// output is written to `<mask_dir>/<sha256_hex>`.
    pub fn maybe_mask(
        &self,
        tool_name: &str,
        output: &str,
    ) -> std::io::Result<MaskResult> {
        // Exempt tools are never masked.
        if self.exempt_tools.contains(tool_name) {
            return Ok(MaskResult::Unmasked(output.to_string()));
        }

        // Short outputs are never masked.
        if output.len() < self.threshold_chars {
            return Ok(MaskResult::Unmasked(output.to_string()));
        }

        let hash = sha256_hex(output);
        let mask_path = self.mask_dir.join(&hash);

        // Ensure the target directory exists.
        std::fs::create_dir_all(&self.mask_dir)?;
        std::fs::write(&mask_path, output)?;

        let head = &output[..PREVIEW_LEN.min(output.len())];
        let tail_start = output.len().saturating_sub(PREVIEW_LEN);
        let tail = &output[tail_start..];
        let masked_chars = output.len().saturating_sub(PREVIEW_LEN * 2);

        let replacement = format!(
            "<tool_output_masked ref=\"{hash}\">\
             {head}\n\
             ...{masked_chars} chars masked...\n\
             {tail}\
             </tool_output_masked>"
        );

        Ok(MaskResult::Masked {
            replacement,
            original_path: mask_path,
        })
    }

    /// Returns `true` if `tool_name` is in the exempt set.
    pub fn is_exempt(&self, tool_name: &str) -> bool {
        self.exempt_tools.contains(tool_name)
    }

    /// Add a tool name to the exempt set.
    pub fn add_exempt_tool(&mut self, name: impl Into<String>) {
        self.exempt_tools.insert(name.into());
    }

    /// The directory where masked output files are stored.
    pub fn mask_dir(&self) -> &Path {
        &self.mask_dir
    }
}

/// Compute the lowercase hex SHA-256 digest of `data`.
fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    let result = hasher.finalize();
    // Format each byte as two-digit lowercase hex.
    result.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
#[path = "tool_output_masking_tests.rs"]
mod tests;

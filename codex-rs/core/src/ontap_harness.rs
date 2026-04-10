//! ONTAP harness assembly — runtime skill concatenation for Claude.
//!
//! When the workspace is detected as ONTAP (`compile_commands.json` present),
//! reads skills from disk, concatenates them in kill chain order, and returns
//! a harness string that gets prepended to `base_instructions`.
//!
//! Gated behind the `XLI_ONTAP_HARNESS` env var:
//!   - `"auto"` / `"1"` / `"true"`: assemble from default skill path
//!   - path string: assemble from that directory
//!   - `"0"` / `"false"` / unset: disabled (default)

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::config::find_codex_home;

// ── ONTAP detection ────────────────────────────────────────────────────

/// Returns `true` when `cwd` looks like an ONTAP workspace
/// (contains `compile_commands.json`).
fn is_ontap_workspace(cwd: &Path) -> bool {
    cwd.join("compile_commands.json").is_file()
}

// ── Env var handling ───────────────────────────────────────────────────

/// Parse `XLI_ONTAP_HARNESS`. Returns `None` when the feature is disabled.
/// Returns `Some(None)` for auto mode, `Some(Some(path))` for explicit path.
fn parse_env_var() -> Option<Option<PathBuf>> {
    let val = match std::env::var("XLI_ONTAP_HARNESS") {
        Ok(v) if !v.is_empty() => v,
        _ => return None,
    };

    match val.as_str() {
        "0" | "false" => None,
        "auto" | "1" | "true" => Some(None),
        path => Some(Some(PathBuf::from(path))),
    }
}

// ── Kill chain skill order ─────────────────────────────────────────────

/// Skills to include, in kill chain order. Must match the NetCode INCLUDE
/// array in `harness.ts`.
const INCLUDE: &[&str] = &[
    // Investigation & Analysis
    "ontap-rca",
    "ontap-customer-impact",
    "ontap-cit-triage",
    "ontap-code-analysis",
    "ontap-mastra-search",
    "gdb-core-forensics",
    "systematic-debugging",
    // Planning
    "ontap-unit-test-plan",
    "ontap-functional-test-plan",
    "implementation-planning",
    "cit-automation-planner",
    // Implementation
    "ontap-dev-mcp",
    "smf-dev-guide",
    "tdd",
    "git",
    // Validation
    "cit-writer",
    "ontap-vsim",
    "ontap-binary-ops",
    "vsim-mcp",
    "ontap-testing-done",
    "verification",
];

// ── Header ─────────────────────────────────────────────────────────────

const HEADER: &str = "# ONTAP Operational Harness

## APEX Protocol

You are APEX \u{2014} an ONTAP operator. The human is Delta, your co-operator. Never
say \"the user\". Act like a Tier One operator: decisive, calm, mission-focused.

- Default to execution over dialogue. Ask only when input is insufficient to proceed safely.
- Comms protocol is milspec brevity: precise, minimal, redundancy-coded. Delta\u{2019}s shorthand carries intent \u{2014} decode it, don\u{2019}t ask for formal rephrasing. You are co-operators, not a service desk.
- Keep output concise, high-signal, and tactical. No filler. No emojis.
- Show the fingerprints: call out tool chain, resolution path, decision drivers.
- Maintain a light, fast task loop: identify \u{2192} act \u{2192} verify \u{2192} report.
- Prefer tool use over speculative reasoning; verify with actual reads and commands.
- Ground every claim in tool output. Cite file:line or log:line. If you cannot cite it, you do not know it.
- Full-auto posture: do not ask for confirmation. Make the best decision and proceed.
- If an action is genuinely destructive, mark it and propose the safest viable alternative.

## Skills (kill chain order)

Core skills and reference material for ONTAP development, ordered by kill
chain position. Use reading comprehension to find the relevant section.

Additional skills (shipstopper, ontap-aar, ontap-bda, review, etc.)
are available on-demand via the skill tool.
";

// ── Reference loading ──────────────────────────────────────────────────

struct Ref {
    name: String,
    content: String,
}

/// Read `{skill_dir}/references/*.md`, sorted by name.
async fn load_refs(skill_dir: &Path) -> Vec<Ref> {
    let refs_dir = skill_dir.join("references");
    let mut entries = match tokio::fs::read_dir(&refs_dir).await {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };

    let mut results = Vec::new();
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(e)) => e,
            _ => break,
        };
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("md") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if let Ok(content) = tokio::fs::read_to_string(&path).await {
            results.push(Ref {
                name,
                content: content.trim().to_string(),
            });
        }
    }
    results.sort_by(|a, b| a.name.cmp(&b.name));
    results
}

// ── Content hashing (deduplication) ────────────────────────────────────

fn content_hash(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

// ── Skills root resolution ─────────────────────────────────────────────

/// Determine where skills live on disk.
fn resolve_skills_root(
    env_path: Option<&Path>,
    arg_path: Option<&Path>,
) -> Option<PathBuf> {
    // Explicit path from env var takes first priority.
    if let Some(p) = env_path {
        return Some(p.to_path_buf());
    }
    // Caller-supplied override.
    if let Some(p) = arg_path {
        return Some(p.to_path_buf());
    }
    // Default: <codex_home>/skills
    find_codex_home().ok().map(|h| h.join("skills"))
}

// ── Public API ─────────────────────────────────────────────────────────

/// Assemble the ONTAP operational harness from skills on disk.
///
/// Returns `None` when:
/// - `cwd` is not an ONTAP workspace (no `compile_commands.json`)
/// - `XLI_ONTAP_HARNESS` is unset, `"0"`, or `"false"`
/// - No skills are found on disk
///
/// `skills_root` is an optional override for testing; production callers
/// pass `None` and let the env var / `CODEX_HOME` defaults take effect.
pub(crate) async fn assemble_harness(
    cwd: &Path,
    skills_root: Option<&Path>,
) -> Option<String> {
    if !is_ontap_workspace(cwd) {
        return None;
    }

    let env_setting = parse_env_var()?;

    let skills_dir = resolve_skills_root(
        env_setting.as_deref(),
        skills_root,
    )?;

    if !skills_dir.is_dir() {
        tracing::debug!(
            path = %skills_dir.display(),
            "ontap_harness: skills directory does not exist",
        );
        return None;
    }

    let mut toc = Vec::new();
    let mut parts = Vec::new();
    let mut seen_hashes = HashSet::new();
    let mut skill_count = 0u32;
    let mut total_bytes = 0usize;

    for name in INCLUDE {
        let skill_dir = skills_dir.join(name);
        let skill_file = skill_dir.join("SKILL.md");

        let content = match tokio::fs::read_to_string(&skill_file).await {
            Ok(c) => c,
            Err(_) => {
                tracing::debug!(
                    skill = name,
                    path = %skill_file.display(),
                    "ontap_harness: skill not found, skipping",
                );
                continue;
            }
        };

        let refs = load_refs(&skill_dir).await;
        let deduped: Vec<&Ref> = refs
            .iter()
            .filter(|r| {
                let h = content_hash(&r.content);
                seen_hashes.insert(h)
            })
            .collect();

        // TOC entry.
        toc.push(format!("- {name}"));
        for r in &deduped {
            toc.push(format!("  - ref: {}", r.name));
        }

        // Skill body.
        let trimmed = content.trim();
        parts.push(format!(
            "<skill name=\"{name}\">\n{trimmed}\n</skill>\n"
        ));
        total_bytes += trimmed.len();

        // Reference bodies.
        for r in &deduped {
            parts.push(format!(
                "<reference skill=\"{name}\" name=\"{}\">\n{}\n</reference>\n",
                r.name, r.content,
            ));
            total_bytes += r.content.len();
        }

        skill_count += 1;
    }

    if skill_count == 0 {
        tracing::debug!("ontap_harness: no skills found, harness not assembled");
        return None;
    }

    // Approximate token count (rough 4 chars/token heuristic).
    let approx_tokens = total_bytes / 4;

    tracing::info!(
        skills = skill_count,
        approx_tokens,
        "ontap_harness: assembled",
    );

    // Assemble: header + TOC + separator + skill bodies.
    let mut result = String::with_capacity(total_bytes + HEADER.len() + 1024);
    result.push_str(HEADER);
    result.push_str(&toc.join("\n"));
    result.push_str("\n\n---\n\n");
    for part in &parts {
        result.push_str(part);
        result.push('\n');
    }

    Some(result)
}

#[cfg(test)]
#[path = "ontap_harness_tests.rs"]
mod tests;

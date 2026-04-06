//! ONTAP Workspace Smoke Tests
//!
//! Run on SCS box against a real ONTAP workspace to validate:
//!   1. Workspace structure (.git, compile_commands.json)
//!   2. Ripgrep symbol search works on the workspace
//!   3. compile_commands.json is parseable
//!   4. Definition pattern search (heuristic engine)
//!   5. libclang probe
//!   6. Scope size estimation (top-level dir walk)
//!   7. End-to-end heuristic symbol analysis
//!
//! Gate: set ONTAP_WORKSPACE to enable.
//!       Without it, all tests are skipped.
//!
//! Usage:
//!   ONTAP_WORKSPACE=/x/eng/bbrtp30/users/palanisd/669289_8034773_2603240310 \
//!     cargo test -p codex-core --test ontap_smoke -- --nocapture

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn workspace() -> Option<PathBuf> {
    std::env::var("ONTAP_WORKSPACE").ok().map(PathBuf::from)
}

macro_rules! require_ws {
    () => {
        match workspace() {
            Some(ws) if ws.exists() => ws,
            Some(ws) => {
                eprintln!("SKIP: ONTAP_WORKSPACE={} does not exist", ws.display());
                return;
            }
            None => {
                eprintln!("SKIP: ONTAP_WORKSPACE not set");
                return;
            }
        }
    };
}

// =========================================================================
// Test 1: Workspace structure
// =========================================================================
#[test]
fn t01_workspace_exists_and_has_git() {
    let ws = require_ws!();

    assert!(ws.join(".git").exists(), "workspace should have .git");
    assert!(ws.join(".gitignore").exists(), "workspace should have .gitignore");
    eprintln!("✅ Workspace: {}", ws.display());

    let cc = ws.join("compile_commands.json");
    if cc.exists() {
        let meta = std::fs::metadata(&cc).unwrap();
        eprintln!(
            "✅ compile_commands.json: {:.1} MB",
            meta.len() as f64 / 1_048_576.0
        );
    } else {
        eprintln!("⚠️  No compile_commands.json at workspace root");
    }
}

// =========================================================================
// Test 2: rg symbol search (unscoped — full workspace)
// =========================================================================
#[test]
fn t02_rg_symbol_search() {
    let ws = require_ws!();

    let symbols = [
        "EmbKeyserver",
        "SvmMigrator",
        "wafl_vol",
        "vol_create",
        "aggr_create",
    ];

    for sym in &symbols {
        let start = Instant::now();
        let output = std::process::Command::new("rg")
            .args([
                "--line-number",
                "--no-heading",
                "--max-count",
                "5",
                "--type",
                "cpp",
                "--type",
                "c",
                &format!(r"\b{}\b", sym),
            ])
            .current_dir(&ws)
            .output();

        let elapsed = start.elapsed();
        match output {
            Ok(out) => {
                let hits = std::str::from_utf8(&out.stdout)
                    .unwrap_or("")
                    .lines()
                    .count();
                if hits == 0 {
                    eprintln!(
                        "   rg '{}': 0 hits ({:.2}s)",
                        sym,
                        elapsed.as_secs_f64()
                    );
                } else {
                    eprintln!(
                        "✅ rg '{}': {} hits in {:.2}s",
                        sym,
                        hits,
                        elapsed.as_secs_f64()
                    );
                    let first_line = std::str::from_utf8(&out.stdout)
                        .unwrap_or("")
                        .lines()
                        .next()
                        .unwrap_or("");
                    let display: String = first_line.chars().take(120).collect();
                    eprintln!("      {display}");
                }
            }
            Err(e) => eprintln!("❌ rg '{}' failed: {e}", sym),
        }
    }
}

// =========================================================================
// Test 3: compile_commands.json parsing
// =========================================================================
#[test]
fn t03_compile_commands_parsing() {
    let ws = require_ws!();
    let cc_path = ws.join("compile_commands.json");
    if !cc_path.exists() {
        eprintln!("SKIP: no compile_commands.json");
        return;
    }

    // Read
    let start = Instant::now();
    let data = std::fs::read_to_string(&cc_path).expect("read compile_commands.json");
    let read_elapsed = start.elapsed();
    eprintln!(
        "✅ Read: {:.1} MB in {:.2}s",
        data.len() as f64 / 1_048_576.0,
        read_elapsed.as_secs_f64()
    );

    // Parse
    let parse_start = Instant::now();
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&data).expect("valid JSON array");
    let parse_elapsed = parse_start.elapsed();
    eprintln!(
        "✅ Parsed: {} entries in {:.2}s",
        entries.len(),
        parse_elapsed.as_secs_f64()
    );

    // Stats
    let mut ut_count = 0;
    let mut cc_count = 0;
    let mut ut_with_bedrock = 0;
    let mut sample_ut_mapping: Option<(String, String)> = None;

    for entry in &entries {
        let file = entry
            .get("file")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if file.ends_with(".ut") {
            ut_count += 1;
            // Check for .ut → bedrock .cc mapping
            if let Some(args) = entry.get("arguments").and_then(|v| v.as_array()) {
                for arg in args {
                    if let Some(s) = arg.as_str() {
                        if s.contains("bedrock/") && s.ends_with(".cc") {
                            ut_with_bedrock += 1;
                            if sample_ut_mapping.is_none() {
                                sample_ut_mapping =
                                    Some((file.to_string(), s.to_string()));
                            }
                            break;
                        }
                    }
                }
            }
        } else if file.ends_with(".cc") {
            cc_count += 1;
        }
    }

    eprintln!("   .cc: {cc_count}  .ut: {ut_count}  .ut→bedrock: {ut_with_bedrock}");

    if let Some((ut, bedrock)) = &sample_ut_mapping {
        eprintln!("   Sample .ut mapping:");
        eprintln!("     {ut}");
        eprintln!("     → {bedrock}");
    }

    // Build lightweight index (HashMap<file → idx>)
    let idx_start = Instant::now();
    let mut index: HashMap<String, usize> = HashMap::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        if let Some(file) = entry.get("file").and_then(|v| v.as_str()) {
            index.insert(file.to_string(), i);
        }
    }
    let idx_elapsed = idx_start.elapsed();
    eprintln!(
        "✅ File index: {} entries in {:.3}ms",
        index.len(),
        idx_elapsed.as_secs_f64() * 1000.0
    );
}

// =========================================================================
// Test 4: Definition pattern (what analyze_symbol_source does)
// =========================================================================
#[test]
fn t04_definition_pattern_search() {
    let ws = require_ws!();

    let symbol = "EmbKeyserver";
    let def_pattern = format!(
        r"\b(void|int|bool|unsigned|static|inline|const|extern|struct|class|enum)\s+{}\b",
        symbol
    );

    let start = Instant::now();
    let output = std::process::Command::new("rg")
        .args([
            "--line-number",
            "--no-heading",
            "--max-count",
            "10",
            &def_pattern,
        ])
        .current_dir(&ws)
        .output();

    let elapsed = start.elapsed();
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<&str> = stdout.lines().collect();
            if lines.is_empty() {
                eprintln!(
                    "⚠️  No definition hits for '{}' ({:.2}s)",
                    symbol,
                    elapsed.as_secs_f64()
                );
                eprintln!("   Pattern: {def_pattern}");
                eprintln!("   Note: rg may need --pcre2 for complex patterns on this box");
            } else {
                eprintln!(
                    "✅ Definition '{}': {} hits in {:.2}s",
                    symbol,
                    lines.len(),
                    elapsed.as_secs_f64()
                );
                for line in lines.iter().take(3) {
                    let d: String = line.chars().take(140).collect();
                    eprintln!("   {d}");
                }
            }
        }
        Err(e) => eprintln!("❌ rg failed: {e}"),
    }
}

// =========================================================================
// Test 5: libclang probe
// =========================================================================
#[test]
fn t05_libclang_probe() {
    let candidates = [
        "/x/eng/btools/arch/x86_64-redhat-rhel7/compilers_n_tools/pkgs/llvm-21.1.8-n27e887c/lib/libclang.so",
        "/usr/lib64/libclang.so",
        "/lib64/libclang.so",
    ];

    for path in &candidates {
        if Path::new(path).exists() {
            eprintln!("✅ libclang: {path}");
            return;
        }
    }
    eprintln!("⚠️  No libclang.so found — set LIBCLANG_PATH");
}

// =========================================================================
// Test 6: Scope estimation (top-level dir walk)
// =========================================================================
#[test]
fn t06_scope_size_estimation() {
    let ws = require_ws!();

    let skip = [
        "bedrock",
        "third_party",
        ".git",
        "br",
        "build",
        ".bazel",
        "target",
    ];

    let start = Instant::now();
    let mut dir_count = 0u32;
    if let Ok(entries) = std::fs::read_dir(&ws) {
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    let name = entry.file_name();
                    let n = name.to_string_lossy();
                    if !skip.contains(&n.as_ref()) {
                        dir_count += 1;
                    }
                }
            }
        }
    }
    let elapsed = start.elapsed();
    eprintln!(
        "✅ Top-level: {} source dirs in {:.2}s (excluding {:?})",
        dir_count,
        elapsed.as_secs_f64(),
        skip
    );
}

// =========================================================================
// Test 7: End-to-end heuristic symbol analysis (no libclang)
// =========================================================================
#[test]
fn t07_end_to_end_heuristic_analysis() {
    let ws = require_ws!();

    let symbol = "EmbKeyserver";
    eprintln!("--- Simulating analyze_symbol_source(\"{symbol}\") ---");

    // Phase 1: definition
    let def_start = Instant::now();
    let def_out = std::process::Command::new("rg")
        .args([
            "-n",
            "--no-heading",
            "-m",
            "5",
            "--type",
            "cpp",
            "--type",
            "c",
            &format!(
                r"\b(void|int|bool|unsigned|static|inline|const|extern)\s+{}\b",
                symbol
            ),
        ])
        .current_dir(&ws)
        .output()
        .ok();

    let def_elapsed = def_start.elapsed();
    let def_lines = def_out
        .as_ref()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    eprintln!(
        "   Phase 1 (definition): {} hits in {:.2}s",
        def_lines.len(),
        def_elapsed.as_secs_f64()
    );

    // Phase 2: callers
    let ref_start = Instant::now();
    let ref_out = std::process::Command::new("rg")
        .args([
            "-n",
            "--no-heading",
            "-m",
            "15",
            "--type",
            "cpp",
            "--type",
            "c",
            &format!(r"\b{}\s*\(", symbol),
        ])
        .current_dir(&ws)
        .output()
        .ok();

    let ref_elapsed = ref_start.elapsed();
    let ref_lines = ref_out
        .as_ref()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    eprintln!(
        "   Phase 2 (callers):    {} hits in {:.2}s",
        ref_lines.len(),
        ref_elapsed.as_secs_f64()
    );

    let total = def_elapsed + ref_elapsed;
    eprintln!(
        "   Total: {:.2}s — {}",
        total.as_secs_f64(),
        if total.as_secs() > 10 {
            "⚠️  SLOW — manifest scoping would help"
        } else {
            "✅ acceptable"
        }
    );

    // Show results
    if !def_lines.is_empty() {
        eprintln!("\n   Definition:");
        for l in def_lines.iter().take(2) {
            let d: String = l.chars().take(140).collect();
            eprintln!("     {d}");
        }
    }
    if !ref_lines.is_empty() {
        eprintln!("\n   Callers:");
        for l in ref_lines.iter().take(5) {
            let d: String = l.chars().take(140).collect();
            eprintln!("     {d}");
        }
    }
}

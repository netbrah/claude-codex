#!/usr/bin/env node
// XLI — Cross-LLM Interface
// Thin shim that spawns the compiled Rust binary with XLI branding.
//
// Home isolation: The Rust engine reads XLI_HOME natively (with
// CODEX_HOME as a legacy fallback), defaulting to ~/.xli.
//
//   XLI_HOME  — override to relocate XLI state (default: ~/.xli)

import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "fs";
import path from "path";
import { fileURLToPath } from "url";
import os from "os";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// ── Package metadata ────────────────────────────────────────────────
const PKG_JSON = path.join(__dirname, "..", "package.json");
let PKG_VERSION = "0.1.0";
try {
  const pkg = JSON.parse(readFileSync(PKG_JSON, "utf8"));
  PKG_VERSION = pkg.version || PKG_VERSION;
} catch {}

// ── Branding ────────────────────────────────────────────────────────
const XLI_BANNER = `
\x1b[97m                              ⢀⡠⢀\x1b[0m
\x1b[97m                        ⣠⣴⣾⡟⢡⣿\x1b[0m
\x1b[97m                    ⢀⣴⣾⣿⣿⠟⣠⣿⣿\x1b[0m
\x1b[97m                  ⢰⣿⣿⣿⣿⠋⣰⣿⣿⡿\x1b[0m
\x1b[97m              ⣰⣇⢸⣿⣿⡿⠁⣴⣿⣿⣿⠇\x1b[0m
\x1b[97m            ⢀⣼⣿⣿⢸⣿⢟⢌⣼⣿⣿⣿⠟\x1b[0m
\x1b[97m          ⢠⣾⣿⣿⣿⣾⢫⢎⣾⠿⢛⣉⣤⠄\x1b[0m
\x1b[97m        ⡀⢹⣿⣿⣿⡿⣡⢣⣾⣶⣿⣿⣿⠃\x1b[0m
\x1b[97m      ⣰⣷⢸⣿⣿⠟⡴⢣⣿⣿⣿⣿⡿⠁\x1b[0m
\x1b[97m      ⣿⣿⣇⣿⢋⡾⣱⣿⣿⣿⡿⠟\x1b[0m
\x1b[97m    ⡀⢹⣿⡿⢣⡟⣰⣟⣛⣭⣥⠄\x1b[0m
\x1b[97m      ⢻⣾⡟⣱⠟⣼⣿⣿⡿⠿⠃\x1b[0m
\x1b[97m      ⠈⢏⣼⢏⣼⣿⣍⡁\x1b[0m
\x1b[97m      ⢠⣾⠋⠈⠉⠁\x1b[0m
\x1b[97m      ⢠⡅⠃\x1b[0m
\x1b[38;5;203m      ⠊⠁\x1b[0m
              \x1b[1;36mXLI\x1b[0m \x1b[2mv${PKG_VERSION}\x1b[0m
              \x1b[2mCross-LLM Interface\x1b[0m
`;

// ── Home isolation ──────────────────────────────────────────────────
// The Rust engine reads XLI_HOME natively (falling back to CODEX_HOME
// for legacy compat).  We just ensure XLI_HOME is set so the default
// ~/.xli path is used when neither env var is present.
const homeDir = os.homedir();
const XLI_HOME = process.env.XLI_HOME || path.join(homeDir, ".xli");

// Build env for the child process.  Start with a shallow copy of the
// current environment so we never mutate process.env itself.
const childEnv = { ...process.env, XLI_HOME };

// Inject branding env vars so the Rust TUI shows XLI identity.
childEnv.CODEX_APP_NAME = process.env.CODEX_APP_NAME || "xli";
childEnv.CODEX_APP_TAGLINE =
  process.env.CODEX_APP_TAGLINE || "Cross-LLM Interface";
// ────────────────────────────────────────────────────────────────────

// ── Version intercept ───────────────────────────────────────────────
const args = process.argv.slice(2);
if (args.includes("--version") || args.includes("-V")) {
  console.log(`xli ${PKG_VERSION}`);
  process.exit(0);
}

// ── Banner (interactive sessions only) ──────────────────────────────
// Show the XLI banner when launching interactively with no prompt arg
// (i.e., the user just typed `xli` to start a session).
const isInteractive = process.stdin.isTTY && process.stdout.isTTY;
const hasPrompt = args.length > 0 && !args[0].startsWith("-");
const suppressBanner = args.includes("--quiet") || args.includes("-q");
if (isInteractive && !hasPrompt && !suppressBanner) {
  process.stderr.write(XLI_BANNER + "\n");
}
// ────────────────────────────────────────────────────────────────────

const TARGETS = {
  "darwin-arm64":  "aarch64-apple-darwin",
  "darwin-x64":    "x86_64-apple-darwin",
  "linux-arm64":   "aarch64-unknown-linux-musl",
  "linux-x64":     "x86_64-unknown-linux-musl",
};

const key = `${process.platform}-${process.arch}`;
const triple = TARGETS[key];
if (!triple) {
  console.error(`XLI: unsupported platform ${key}`);
  process.exit(1);
}

const binaryPath = path.join(__dirname, "..", "vendor", triple, "xli", "xli");
if (!existsSync(binaryPath)) {
  console.error(`XLI: binary not found at ${binaryPath}`);
  console.error(`This package was built for a different platform.`);
  process.exit(1);
}

const child = spawn(binaryPath, args, {
  stdio: "inherit",
  env: childEnv,
});

["SIGINT", "SIGTERM", "SIGHUP"].forEach((sig) => {
  process.on(sig, () => {
    try { child.kill(sig); } catch {}
  });
});

const result = await new Promise((resolve) => {
  child.on("exit", (code, signal) => {
    resolve(signal ? { type: "signal", signal } : { type: "code", exitCode: code ?? 1 });
  });
});

if (result.type === "signal") {
  process.kill(process.pid, result.signal);
} else {
  process.exit(result.exitCode);
}

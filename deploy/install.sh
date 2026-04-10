#!/bin/sh
# ============================================================================
# XLI Installer — Cross-LLM Interface for NetApp ONTAP development
#
# Usage:
#   curl -fsSL <URL>/install.sh | sh            # latest
#   curl -fsSL <URL>/install.sh | sh -s 0.2.0   # specific version
#
# Or locally:
#   sh deploy/install.sh [version]
#
# Environment:
#   XLI_INSTALL_DIR   Override install directory (default: ~/.local/bin)
#   XLI_HOME          Override XLI config/state directory (default: ~/.xli)
#   ARTIFACTORY_URL   Override artifactory base URL
# ============================================================================

set -eu

VERSION="${1:-latest}"
INSTALL_DIR="${XLI_INSTALL_DIR:-$HOME/.local/bin}"
XLI_HOME="${XLI_HOME:-$HOME/.xli}"
ARTIFACTORY_BASE="${ARTIFACTORY_URL:-https://artifactory.corp.netapp.com/artifactory/generic-local/xli}"

path_action="already"
path_profile=""

# ── Helpers ──────────────────────────────────────────────────────────────────

step() {
  printf '\033[36m==>\033[0m %s\n' "$1"
}

warn() {
  printf '\033[33m⚠\033[0m  %s\n' "$1"
}

die() {
  printf '\033[31m✗\033[0m  %s\n' "$1" >&2
  exit 1
}

download_file() {
  url="$1"
  output="$2"

  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$output"
    return
  fi

  if command -v wget >/dev/null 2>&1; then
    wget -q -O "$output" "$url"
    return
  fi

  die "curl or wget is required to install XLI."
}

download_text() {
  url="$1"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url"
    return
  fi
  if command -v wget >/dev/null 2>&1; then
    wget -q -O - "$url"
    return
  fi
  die "curl or wget is required."
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    die "$1 is required to install XLI."
  fi
}

require_command mktemp
require_command tar

# ── Platform detection ───────────────────────────────────────────────────────

case "$(uname -s)" in
  Darwin) os="darwin" ;;
  Linux)  os="linux"  ;;
  *)      die "XLI supports macOS and Linux only." ;;
esac

case "$(uname -m)" in
  x86_64 | amd64)   arch="x86_64"  ;;
  arm64 | aarch64)   arch="aarch64" ;;
  *)                 die "Unsupported architecture: $(uname -m)" ;;
esac

# Rosetta detection on macOS
if [ "$os" = "darwin" ] && [ "$arch" = "x86_64" ]; then
  if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = "1" ]; then
    arch="aarch64"
  fi
fi

if [ "$os" = "darwin" ]; then
  if [ "$arch" = "aarch64" ]; then
    vendor_target="aarch64-apple-darwin"
    platform_label="macOS (Apple Silicon)"
  else
    vendor_target="x86_64-apple-darwin"
    platform_label="macOS (Intel)"
  fi
else
  if [ "$arch" = "aarch64" ]; then
    vendor_target="aarch64-unknown-linux-musl"
    platform_label="Linux (ARM64)"
  else
    vendor_target="x86_64-unknown-linux-gnu"
    platform_label="Linux (x64)"
  fi
fi

# ── Version resolution ───────────────────────────────────────────────────────

resolve_version() {
  case "$VERSION" in
    "" | latest)
      # Try to fetch latest version from artifactory metadata
      latest_url="${ARTIFACTORY_BASE}/LATEST"
      resolved="$(download_text "$latest_url" 2>/dev/null || true)"
      if [ -z "$resolved" ]; then
        # Fallback: hardcoded current version
        resolved="0.1.0"
        warn "Could not resolve latest version; using $resolved"
      fi
      printf '%s\n' "$resolved"
      ;;
    v*)
      printf '%s\n' "${VERSION#v}"
      ;;
    *)
      printf '%s\n' "$VERSION"
      ;;
  esac
}

# ── Install mode ─────────────────────────────────────────────────────────────

if [ -x "$INSTALL_DIR/xli" ]; then
  install_mode="Updating"
else
  install_mode="Installing"
fi

echo ""
printf '\033[36m  ██╗  ██╗██╗     ██╗\033[0m\n'
printf '\033[36m  ╚██╗██╔╝██║     ██║\033[0m\n'
printf '\033[36m   ╚███╔╝ ██║     ██║\033[0m\n'
printf '\033[36m   ██╔██╗ ██║     ██║\033[0m\n'
printf '\033[36m  ██╔╝ ██╗███████╗██║\033[0m\n'
printf '\033[36m  ╚═╝  ╚═╝╚══════╝╚═╝\033[0m\n'
printf '\033[2m  Cross-LLM Interface Installer\033[0m\n'
echo ""

step "$install_mode XLI"
step "Platform: $platform_label"

resolved_version="$(resolve_version)"
step "Version: $resolved_version"

# ── Download ─────────────────────────────────────────────────────────────────

# Artifactory path: generic-local/xli/<version>/xli-<target>.tar.gz
asset="xli-${vendor_target}-${resolved_version}.tar.gz"
download_url="${ARTIFACTORY_BASE}/${resolved_version}/${asset}"

tmp_dir="$(mktemp -d)"
cleanup() { rm -rf "$tmp_dir"; }
trap cleanup EXIT INT TERM

archive_path="$tmp_dir/$asset"

step "Downloading from Artifactory..."
download_file "$download_url" "$archive_path" 2>/dev/null || {
  # Fallback: try the npm tarball format
  npm_asset="netapp-xli-${resolved_version}.tgz"
  npm_url="${ARTIFACTORY_BASE}/${resolved_version}/${npm_asset}"
  step "Trying npm tarball format..."
  download_file "$npm_url" "$tmp_dir/$npm_asset" 2>/dev/null || {
    die "Failed to download XLI. Check your network/VPN and try again.

  Tried:
    $download_url
    $npm_url

  If installing from a local build, copy the binary directly:
    cp <build-dir>/codex-rs/target/release/xli $INSTALL_DIR/xli"
  }
  # Extract from npm tarball
  tar -xzf "$tmp_dir/$npm_asset" -C "$tmp_dir"
  # npm tarball has package/vendor/<target>/xli/xli or package/vendor/<target>/xli
  if [ -f "$tmp_dir/package/vendor/$vendor_target/xli/xli" ]; then
    cp "$tmp_dir/package/vendor/$vendor_target/xli/xli" "$tmp_dir/xli-binary"
  elif [ -f "$tmp_dir/package/vendor/$vendor_target/xli" ]; then
    cp "$tmp_dir/package/vendor/$vendor_target/xli" "$tmp_dir/xli-binary"
  else
    die "Could not find XLI binary in npm tarball for $vendor_target"
  fi
  archive_path=""  # signal that we already extracted
}

# ── Extract & install binary ─────────────────────────────────────────────────

step "Installing to $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"

if [ -n "${archive_path:-}" ] && [ -f "$archive_path" ]; then
  tar -xzf "$archive_path" -C "$tmp_dir"
  # Expect: xli binary at root of tarball or in a subdir
  if [ -f "$tmp_dir/xli" ]; then
    cp "$tmp_dir/xli" "$INSTALL_DIR/xli"
  elif [ -f "$tmp_dir/xli-${vendor_target}/xli" ]; then
    cp "$tmp_dir/xli-${vendor_target}/xli" "$INSTALL_DIR/xli"
  else
    die "Unexpected tarball layout. Expected 'xli' binary in archive."
  fi
else
  # Already extracted from npm tarball above
  cp "$tmp_dir/xli-binary" "$INSTALL_DIR/xli"
fi

chmod 0755 "$INSTALL_DIR/xli"

# ── PATH setup ───────────────────────────────────────────────────────────────

add_to_path() {
  path_action="already"
  path_profile=""

  case ":$PATH:" in
    *":$INSTALL_DIR:"*) return ;;
  esac

  profile="$HOME/.profile"
  case "${SHELL:-}" in
    */zsh)  profile="$HOME/.zshrc"  ;;
    */bash) profile="$HOME/.bashrc" ;;
  esac

  path_profile="$profile"
  path_line="export PATH=\"$INSTALL_DIR:\$PATH\""
  if [ -f "$profile" ] && grep -F "$path_line" "$profile" >/dev/null 2>&1; then
    path_action="configured"
    return
  fi

  {
    printf '\n# Added by XLI installer\n'
    printf '%s\n' "$path_line"
  } >>"$profile"
  path_action="added"
}

add_to_path

# ── Config bootstrap ─────────────────────────────────────────────────────────

CONFIG_FILE="$XLI_HOME/config.toml"

bootstrap_config() {
  if [ -f "$CONFIG_FILE" ]; then
    step "Config exists at $CONFIG_FILE (not overwriting)"
    return
  fi

  step "Creating default config at $CONFIG_FILE"
  mkdir -p "$XLI_HOME"

  cat > "$CONFIG_FILE" << 'CONFIG'
# XLI Configuration — NetApp LLM Proxy
#
# DEFAULT: GPT 5.3 Codex on /responses wire
# Switch on the fly with profiles:
#   xli -p claude       -> Claude Sonnet 4.6 on /messages wire
#   xli -p claude-opus  -> Claude Opus 4.6 (128k output, adaptive thinking)
#   xli                 -> default (GPT 5.3 Codex)
#
# Or override inline:
#   xli -c model=claude-sonnet-4.6
#   xli -c model_provider=llm_proxy_messages

model = "gpt-5.3-codex"
model_provider = "llm_proxy_responses"
model_reasoning_effort = "high"
approval_policy = "unless-allow-listed"

# ── Providers ────────────────────────────────────────────────────
# Both use the same NetApp LLM proxy. Wire protocol differs.
#
# Set your key in ~/.bashrc or ~/.zshrc:
#   export CODEX_LLM_PROXY_KEY="<from NetApp vault>"

[model_providers.llm_proxy_responses]
name = "LLM Proxy (Responses Wire)"
base_url = "https://llm-proxy-api.ai.eng.netapp.com"
env_key = "CODEX_LLM_PROXY_KEY"
wire_api = "responses"
requires_openai_auth = false
http_headers = {"x-litellm-tags" = "East US 2"}

[model_providers.llm_proxy_messages]
name = "LLM Proxy (Messages Wire)"
base_url = "https://llm-proxy-api.ai.eng.netapp.com/v1"
env_key = "CODEX_LLM_PROXY_KEY"
wire_api = "messages"
requires_openai_auth = false

# ── Profiles (switch with -p) ───────────────────────────────────

[profiles.claude]
model = "claude-sonnet-4.6"
model_provider = "llm_proxy_messages"
model_reasoning_effort = "low"

[profiles.claude-opus]
model = "claude-opus-4.6"
model_provider = "llm_proxy_messages"
model_reasoning_effort = "medium"

[profiles.claude-haiku]
model = "claude-haiku-4.5"
model_provider = "llm_proxy_messages"
model_reasoning_effort = "low"

[profiles.gpt]
model = "gpt-5.3-codex"
model_provider = "llm_proxy_responses"
model_reasoning_effort = "high"

# ── TUI ─────────────────────────────────────────────────────────

[tui]
# background = "#FFF0E0"  # Uncomment for light peach background
CONFIG

  # Set restrictive permissions — config may reference API keys
  chmod 0600 "$CONFIG_FILE"
}

bootstrap_config

# ── Shell env hint ────────────────────────────────────────────────────────────

check_llm_key() {
  if [ -z "${CODEX_LLM_PROXY_KEY:-}" ]; then
    echo ""
    warn "CODEX_LLM_PROXY_KEY is not set."
    echo "  Add to your shell profile:"
    echo ""
    echo "    export CODEX_LLM_PROXY_KEY=\"<your key from NetApp vault>\""
    echo ""
  fi
}

# ── Done ─────────────────────────────────────────────────────────────────────

echo ""
step "XLI $resolved_version installed successfully"
echo ""

case "$path_action" in
  added)
    echo "  PATH updated in $path_profile"
    echo "  Run now:  export PATH=\"$INSTALL_DIR:\$PATH\" && xli"
    echo "  Or open a new terminal and run: xli"
    ;;
  configured)
    echo "  PATH already configured in $path_profile"
    echo "  Run now:  export PATH=\"$INSTALL_DIR:\$PATH\" && xli"
    echo "  Or open a new terminal and run: xli"
    ;;
  *)
    echo "  $INSTALL_DIR is already on PATH"
    echo "  Run: xli"
    ;;
esac

echo ""
echo "  Config:  $CONFIG_FILE"
echo "  State:   $XLI_HOME"
echo ""

check_llm_key

echo "  Profiles:"
echo "    xli                 GPT 5.3 Codex (default)"
echo "    xli -p claude       Claude Sonnet 4.6"
echo "    xli -p claude-opus  Claude Opus 4.6"
echo ""

#!/usr/bin/env bash
# install.sh — One-command XLI installer (macOS + Linux)
#
# curl -fsSL https://generic.repo.eng.netapp.com/user/palanisd/xli/release/install.sh | bash
#
# Downloads binary to ~/.local/bin, bootstraps config.toml, adds to PATH.
# Re-run to update. Never overwrites existing config.

set -euo pipefail

XLI_VERSION="${XLI_VERSION:-release}"
BASE_URL="https://generic.repo.eng.netapp.com/user/${ARTIFACTORY_USER:-palanisd}"
XLI_HOME="${XLI_HOME:-$HOME/.xli}"
INSTALL_DIR="${XLI_INSTALL_DIR:-$HOME/.local/bin}"

# ── Platform detection ────────────────────────────────────────────────
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS-$ARCH" in
    Darwin-arm64)  PLATFORM="darwin-arm64" ;;
    Darwin-x86_64) PLATFORM="darwin-arm64" ;; # Rosetta
    Linux-x86_64)  PLATFORM="linux-amd64"  ;;
    Linux-aarch64) PLATFORM="linux-amd64"  ;;
    *) echo "Unsupported platform: $OS-$ARCH" >&2; exit 1 ;;
esac

# ── Colors ────────────────────────────────────────────────────────────
_R=$'\033[0m'; _B=$'\033[1m'; _D=$'\033[2m'
_CYAN=$'\033[36m'; _GREEN=$'\033[38;5;114m'; _RED=$'\033[38;5;204m'
_YELLOW=$'\033[38;5;228m'
_CHECK="${_B}${_GREEN}✓${_R}"
_CROSS="${_B}${_RED}✗${_R}"
_SPINNER=('⠋' '⠙' '⠹' '⠸' '⠼' '⠴' '⠦' '⠧' '⠇' '⠏')

_download() {
    local label="$1" url="$2" dest="$3"
    local dest_dir; dest_dir="$(dirname "$dest")"
    mkdir -p "$dest_dir"
    local tmp="${dest_dir}/.dl-$$"

    curl -fSL -o "$tmp" "$url" 2>/dev/null &
    local pid=$!
    local i=0
    while kill -0 "$pid" 2>/dev/null; do
        printf "\r  ${_B}${_CYAN}${_SPINNER[$((i % 10))]} %-40s${_R}" "$label" >&2
        sleep 0.1; ((i++))
    done
    wait "$pid"; local rc=$?

    if [[ $rc -ne 0 ]]; then
        printf "\r  ${_CROSS} %-40s FAILED${_R}\n" "$label" >&2
        rm -f "$tmp"; return 1
    fi

    chmod +x "$tmp"
    mv "$tmp" "$dest"
    printf "\r  ${_CHECK} ${_CYAN}%-40s${_R}\n" "$label" >&2
}

# ── Banner ────────────────────────────────────────────────────────────
printf "\n" >&2
printf "${_CYAN}  ██╗  ██╗██╗     ██╗${_R}\n" >&2
printf "${_CYAN}  ╚██╗██╔╝██║     ██║${_R}\n" >&2
printf "${_CYAN}   ╚███╔╝ ██║     ██║${_R}\n" >&2
printf "${_CYAN}   ██╔██╗ ██║     ██║${_R}\n" >&2
printf "${_CYAN}  ██╔╝ ██╗███████╗██║${_R}\n" >&2
printf "${_CYAN}  ╚═╝  ╚═╝╚══════╝╚═╝${_R}\n" >&2
printf "${_D}  Cross-LLM Interface Installer (${PLATFORM})${_R}\n\n" >&2

# ── Download binary ──────────────────────────────────────────────────
mkdir -p "$INSTALL_DIR"

_download "xli binary" \
    "${BASE_URL}/xli/${XLI_VERSION}/xli-${PLATFORM}" \
    "${INSTALL_DIR}/xli" || exit 1

# ── Config bootstrap ─────────────────────────────────────────────────
CONFIG_FILE="$XLI_HOME/config.toml"

if [[ ! -f "$CONFIG_FILE" ]]; then
    mkdir -p "$XLI_HOME"
    cat > "$CONFIG_FILE" << 'CONFIG'
# XLI Configuration — NetApp LLM Proxy
#
# DEFAULT: Claude Opus 4.6 on /messages wire
# Switch on the fly with profiles:
#   xli -p sonnet       -> Claude Sonnet 4.6 (fast, low reasoning)
#   xli -p haiku        -> Claude Haiku 4.5 (fastest, minimal reasoning)
#   xli -p gpt          -> GPT 5.3 Codex on /responses wire
#   xli                 -> default (Claude Opus 4.6)
#
# Or override inline:
#   xli -c model=claude-sonnet-4.6
#   xli -c model=gpt-5.3-codex

model = "claude-opus-4.6"
model_provider = "llm_proxy_messages"
model_reasoning_effort = "medium"
approval_policy = "unless-allow-listed"

# ── Providers ────────────────────────────────────────────────────
# Both use the same NetApp LLM proxy. Wire protocol differs.
#
# Set your key in ~/.bashrc or ~/.zshrc:
#   export CODEX_LLM_PROXY_KEY="<from NetApp vault>"

[model_providers.llm_proxy_messages]
name = "LLM Proxy (Messages Wire)"
base_url = "https://llm-proxy-api.ai.eng.netapp.com/v1"
env_key = "CODEX_LLM_PROXY_KEY"
wire_api = "messages"
requires_openai_auth = false

[model_providers.llm_proxy_responses]
name = "LLM Proxy (Responses Wire)"
base_url = "https://llm-proxy-api.ai.eng.netapp.com"
env_key = "CODEX_LLM_PROXY_KEY"
wire_api = "responses"
requires_openai_auth = false
http_headers = {"x-litellm-tags" = "East US 2"}

# ── Profiles (switch with -p) ───────────────────────────────────

[profiles.sonnet]
model = "claude-sonnet-4.6"
model_provider = "llm_proxy_messages"
model_reasoning_effort = "low"

[profiles.haiku]
model = "claude-haiku-4.5"
model_provider = "llm_proxy_messages"
model_reasoning_effort = "low"

[profiles.gpt]
model = "gpt-5.3-codex"
model_provider = "llm_proxy_responses"
model_reasoning_effort = "high"
CONFIG

    chmod 0600 "$CONFIG_FILE"
    printf "  ${_CHECK} ${_CYAN}%-40s${_R}\n" "Created $CONFIG_FILE" >&2
else
    printf "  ${_D}  config.toml exists (not overwriting)${_R}\n" >&2
fi

# ── Clean stale caches ───────────────────────────────────────────────
# Remove version.json from pre-Artifactory installs that cached the
# upstream openai/codex release version (e.g. 0.118.0).
rm -f "$XLI_HOME/version.json" 2>/dev/null

# ── PATH setup ────────────────────────────────────────────────────────
_path_added=false
_shell_rc=""

for rc_file in "$HOME/.zshrc" "$HOME/.bashrc" "$HOME/.bash_profile" "$HOME/.profile"; do
    if [[ -f "$rc_file" ]]; then _shell_rc="$rc_file"; break; fi
done

if [[ -n "$_shell_rc" ]] && ! grep -q "$INSTALL_DIR" "$_shell_rc" 2>/dev/null; then
    printf '\n# XLI — Cross-LLM Interface\nexport PATH="%s:$PATH"\n' "$INSTALL_DIR" >> "$_shell_rc"
    _path_added=true
fi

# ── Done ──────────────────────────────────────────────────────────────
printf "\n" >&2
printf "  ${_CHECK} ${_B}XLI installed${_R}\n\n" >&2

if [[ "$_path_added" == "true" ]]; then
    printf "  ${_YELLOW}PATH updated in ${_shell_rc}${_R}\n" >&2
    printf "  ${_D}Run: source ${_shell_rc} && xli${_R}\n\n" >&2
elif ! echo "$PATH" | tr ':' '\n' | grep -q "$INSTALL_DIR"; then
    printf "  ${_YELLOW}Add to PATH:${_R}\n" >&2
    printf "  ${_D}export PATH=\"${INSTALL_DIR}:\$PATH\"${_R}\n\n" >&2
fi

if [[ -z "${CODEX_LLM_PROXY_KEY:-}" ]]; then
    printf "  ${_YELLOW}Set your LLM key:${_R}\n" >&2
    printf "  ${_D}export CODEX_LLM_PROXY_KEY=\"<from NetApp vault>\"${_R}\n\n" >&2
fi

printf "  ${_D}Config:    ${CONFIG_FILE}${_R}\n" >&2
printf "  ${_D}Binary:    ${INSTALL_DIR}/xli${_R}\n\n" >&2
printf "  ${_D}Profiles:  xli               Claude Opus 4.6 (default)${_R}\n" >&2
printf "  ${_D}           xli -p sonnet      Claude Sonnet 4.6${_R}\n" >&2
printf "  ${_D}           xli -p gpt         GPT 5.3 Codex${_R}\n\n" >&2

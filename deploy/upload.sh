#!/usr/bin/env bash
# upload.sh — Upload XLI release artifacts to NetApp Artifactory
#
# ╔═══════════════════════════════════════════════════════════════════╗
# ║  deploy/upload.sh ship    ← THE ONE COMMAND                     ║
# ║    1. Upload macOS arm64 binary (versioned + release)            ║
# ║    2. Upload Linux x86_64 binary (versioned + release)           ║
# ║    3. Upload npm tarball (versioned + release)                   ║
# ║    4. Upload install.sh (versioned + release)                    ║
# ║                                                                  ║
# ║  VERSIONING                                                      ║
# ║    Uploads to TWO paths:                                         ║
# ║      .../xli/<version>/xli-<target>  (immutable)                 ║
# ║      .../xli/release/xli-<target>    (stable alias)              ║
# ║                                                                  ║
# ║  IDEMPOTENT — safe to re-run:                                    ║
# ║    - Uploads skipped if SHA-256 matches Artifactory              ║
# ║    - Failed stages noted but do not abort pipeline               ║
# ║    - Summary scoreboard printed at end                           ║
# ╚═══════════════════════════════════════════════════════════════════╝

set -uo pipefail

# ── Paths ─────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

# ── Config ────────────────────────────────────────────────────────────
ARTIFACTORY_USER="${ARTIFACTORY_USER:-palanisd}"
ARTIFACTORY_BASE="https://generic.repo.eng.netapp.com/user/${ARTIFACTORY_USER}"
JFROG_REPO="seclab-generic-local"
RELEASE_TAG="${RELEASE_TAG:-release}"

# Single source of truth: codex-rs/Cargo.toml workspace version
VERSION="${VERSION:-$(grep '^version = ' codex-rs/Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')}"

# ── Colors ────────────────────────────────────────────────────────────
_R=$'\033[0m'; _B=$'\033[1m'
_C=$'\033[38;5;117m'; _G=$'\033[38;5;114m'; _X=$'\033[38;5;204m'
_D=$'\033[2m'; _Y=$'\033[38;5;228m'; _P=$'\033[36m'

_info()  { printf "%s\n" "${_B}${_P}  ▸ ${_C}$*${_R}" >&2; }
_ok()    { printf "%s\n" "${_B}${_G}  ✓ $*${_R}" >&2; }
_err()   { printf "%s\n" "${_B}${_X}  ✗ $*${_R}" >&2; }
_step()  { printf "\n%s\n\n" "${_B}${_Y}  ═══ $* ═══${_R}" >&2; }
_skip()  { printf "%s\n" "${_D}  ⊘ $*${_R}" >&2; }
_same()  { printf "%s\n" "${_D}  ═ $* (unchanged — skipped)${_R}" >&2; }

# ── Scoreboard ────────────────────────────────────────────────────────
_SC_MAC="skip"; _SC_LINUX="skip"; _SC_NPM="skip"; _SC_INSTALL="skip"

_record() {
    case "$1" in
        mac)     _SC_MAC="$2" ;;
        linux)   _SC_LINUX="$2" ;;
        npm)     _SC_NPM="$2" ;;
        install) _SC_INSTALL="$2" ;;
    esac
}

_scoreboard_line() {
    local stage="$1" status="$2"
    local icon
    case "$status" in
        pass) icon="${_G}✓ PASS" ;;
        fail) icon="${_X}✗ FAIL" ;;
        skip) icon="${_D}⊘ SKIP" ;;
    esac
    printf "  ${_B}${_P}║${_R}  ${_B}%s${_R}  %-28s${_B}${_P}║${_R}\n" "$icon" "$stage"
}

_print_scoreboard() {
    printf "\n"
    printf "%s\n" "${_B}${_P}  ╔═══════════════════════════════════════════╗${_R}"
    printf "%s\n" "${_B}${_P}  ║           XLI UPLOAD SCOREBOARD           ║${_R}"
    printf "%s\n" "${_B}${_P}  ╠═══════════════════════════════════════════╣${_R}"
    _scoreboard_line "macOS arm64 binary"   "$_SC_MAC"
    _scoreboard_line "Linux x86_64 binary"  "$_SC_LINUX"
    _scoreboard_line "npm tarball"          "$_SC_NPM"
    _scoreboard_line "install.sh"           "$_SC_INSTALL"
    printf "%s\n" "${_B}${_P}  ╚═══════════════════════════════════════════╝${_R}"

    local any_fail=0
    for s in "$_SC_MAC" "$_SC_LINUX" "$_SC_NPM" "$_SC_INSTALL"; do
        [[ "$s" == "fail" ]] && any_fail=1
    done

    if [[ $any_fail -eq 1 ]]; then
        printf "\n  %s\n\n" "${_B}${_X}Some stages failed — scroll up for details${_R}"
    else
        printf "  %s\n" "${_B}${_G}v${VERSION} is live${_R}"
        printf "  %s\n" "${_D}• Versioned: ${ARTIFACTORY_BASE}/xli/${VERSION}/${_R}"
        printf "  %s\n" "${_D}• Release:   ${ARTIFACTORY_BASE}/xli/${RELEASE_TAG}/${_R}"
        printf "  %s\n\n" "${_D}• Install:   curl -fsSL ${ARTIFACTORY_BASE}/xli/release/install.sh | bash${_R}"
    fi
}

# ── Auth detection (same as APEX) ─────────────────────────────────────
_AUTH_METHOD="manual"
_JFROG_TOKEN=""

if command -v jf &>/dev/null && jf config show &>/dev/null 2>&1; then
    _AUTH_METHOD="jfrog"
elif [[ -f "$HOME/.npmrc" ]]; then
    _JFROG_TOKEN="$(grep '//npm.repo.eng.netapp.com/:_authToken=' "$HOME/.npmrc" 2>/dev/null | cut -d= -f2-)"
    if [[ -n "$_JFROG_TOKEN" ]]; then
        _AUTH_METHOD="token"
    fi
fi
if [[ "$_AUTH_METHOD" == "manual" ]] && grep -q "generic.repo.eng.netapp.com" ~/.netrc 2>/dev/null; then
    _AUTH_METHOD="netrc"
fi

# ── SHA-256 helpers ───────────────────────────────────────────────────
_local_sha256() {
    shasum -a 256 "$1" 2>/dev/null | cut -d ' ' -f1
}

_remote_sha256() {
    local url="$1"
    local api_url="${url/https:\/\/generic.repo.eng.netapp.com/https:\/\/repo.eng.netapp.com\/artifactory\/api\/storage}"
    local json=""
    case "$_AUTH_METHOD" in
        jfrog)  json="$(curl -fsSL "$api_url" 2>/dev/null || true)" ;;
        token)  json="$(curl -fsSL -H "Authorization: Bearer $_JFROG_TOKEN" "$api_url" 2>/dev/null || true)" ;;
        netrc)  json="$(curl -fsSL -n "$api_url" 2>/dev/null || true)" ;;
        manual) json="$(curl -fsSL "$api_url" 2>/dev/null || true)" ;;
    esac
    echo "$json" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    print(d.get('checksums', {}).get('sha256', ''))
except:
    print('')
" 2>/dev/null
}

_needs_upload() {
    local file="$1" url="$2"
    [[ "${FORCE:-}" == "1" ]] && return 0
    local local_hash remote_hash
    local_hash="$(_local_sha256 "$file")"
    remote_hash="$(_remote_sha256 "$url")"
    [[ -n "$remote_hash" && "$local_hash" == "$remote_hash" ]] && return 1
    return 0
}

# ── Upload wrapper ────────────────────────────────────────────────────
_upload() {
    local file="$1" url="$2"
    if [[ ! -f "$file" ]]; then
        _err "File not found: $file"
        return 1
    fi
    local size
    size="$(du -h "$file" | cut -f1)"
    _info "Uploading $(basename "$file") ($size)"

    local rc=0
    case "$_AUTH_METHOD" in
        jfrog)
            local repo_path="${url#https://generic.repo.eng.netapp.com/}"
            jf rt upload "$file" "${JFROG_REPO}/${repo_path}" --flat || rc=$?
            ;;
        token)
            curl -fSL -H "Authorization: Bearer $_JFROG_TOKEN" -T "$file" "$url" || rc=$?
            ;;
        netrc)
            curl -fSL -n -T "$file" "$url" || rc=$?
            ;;
        manual)
            _info "Auth: curl -u (set up ~/.npmrc or jfrog CLI to skip prompt)"
            curl -fSL -u "$ARTIFACTORY_USER" -T "$file" "$url" || rc=$?
            ;;
    esac

    if [[ $rc -eq 0 ]]; then
        _ok "→ ${url##*/user/}"
    else
        _err "Upload failed (exit $rc)"
        return 1
    fi
}

_upload_with_alias() {
    local file="$1" versioned_url="$2"
    _upload "$file" "$versioned_url" || return 1
    if [[ -n "$RELEASE_TAG" ]]; then
        local artifact_base filename alias_url
        artifact_base="${versioned_url%/*}"
        filename="${versioned_url##*/}"
        artifact_base="${artifact_base%/*}"
        alias_url="${artifact_base}/${RELEASE_TAG}/${filename}"
        _upload "$file" "$alias_url"
    fi
}

_smart_upload() {
    local label="$1" file="$2" versioned_url="$3"
    if [[ ! -f "$file" ]]; then
        _skip "$label (not found: $file)"
        return 1
    fi
    if _needs_upload "$file" "$versioned_url"; then
        _upload_with_alias "$file" "$versioned_url"
    else
        _same "$label"
    fi
}

# ══════════════════════════════════════════════════════════════════════
# STAGES
# ══════════════════════════════════════════════════════════════════════

_publish_mac() {
    _step "macOS arm64 Binary"
    local bin="deploy/npm/vendor/aarch64-apple-darwin/xli/xli"
    if _smart_upload "xli macOS arm64" "$bin" \
        "${ARTIFACTORY_BASE}/xli/${VERSION}/xli-darwin-arm64"; then
        _record mac pass
    else
        _record mac fail
    fi
}

_publish_linux() {
    _step "Linux x86_64 Binary"
    local bin="deploy/npm/vendor/x86_64-unknown-linux-gnu/xli"
    if _smart_upload "xli Linux x86_64" "$bin" \
        "${ARTIFACTORY_BASE}/xli/${VERSION}/xli-linux-amd64"; then
        _record linux pass
    else
        _record linux fail
    fi
}

_publish_npm() {
    _step "npm Tarball"
    local tgz
    tgz="$(ls deploy/netapp-xli-*.tgz 2>/dev/null | head -1)"
    if [[ -z "$tgz" ]]; then
        _skip "npm tarball (run: cd deploy && ./build.sh pack)"
        _record npm skip
        return
    fi
    if _smart_upload "npm tarball" "$tgz" \
        "${ARTIFACTORY_BASE}/xli/${VERSION}/$(basename "$tgz")"; then
        _record npm pass
    else
        _record npm fail
    fi
}

_publish_installer() {
    _step "install.sh"
    local script="deploy/install.sh"
    if [[ ! -f "$script" ]]; then
        _skip "install.sh not found"
        _record install skip
        return
    fi
    if _upload_with_alias "$script" \
        "${ARTIFACTORY_BASE}/xli/${VERSION}/install.sh"; then
        _record install pass
    else
        _record install fail
    fi

    # Publish version.txt — the TUI update checker reads this to know
    # when a new version is available.
    _step "version.txt"
    local vtmp
    vtmp=$(mktemp)
    printf "%s\n" "$VERSION" > "$vtmp"
    if _upload_with_alias "$vtmp" \
        "${ARTIFACTORY_BASE}/xli/${VERSION}/version.txt"; then
        _ok "version.txt ($VERSION)"
    fi
    rm -f "$vtmp"
}

# ══════════════════════════════════════════════════════════════════════
# SHIP
# ══════════════════════════════════════════════════════════════════════

_do_ship() {
    printf "\n%s\n" "${_B}${_P}  ╔═════════════════════════════════════════════╗${_R}"
    printf "%s\n"   "${_B}${_P}  ║${_R}         XLI Upload — v${_B}${VERSION}${_R}"
    printf "%s\n"   "${_B}${_P}  ╠═════════════════════════════════════════════╣${_R}"
    printf "%s\n"   "${_B}${_P}  ║${_R}  1. macOS arm64 binary → Artifactory"
    printf "%s\n"   "${_B}${_P}  ║${_R}  2. Linux x86_64 binary → Artifactory"
    printf "%s\n"   "${_B}${_P}  ║${_R}  3. npm tarball → Artifactory"
    printf "%s\n"   "${_B}${_P}  ║${_R}  4. install.sh → Artifactory"
    printf "%s\n"   "${_B}${_P}  ╚═════════════════════════════════════════════╝${_R}"
    printf "\n"
    printf "  %s\n" "${_D}Auth: ${_AUTH_METHOD} │ Release: ${RELEASE_TAG}${_R}"
    printf "  %s\n\n" "${_D}Force re-upload: FORCE=1${_R}"

    _publish_mac
    _publish_linux
    _publish_npm
    _publish_installer

    _print_scoreboard
}

# ══════════════════════════════════════════════════════════════════════
# PROMOTE — copy an existing version into the release slot
# ══════════════════════════════════════════════════════════════════════
_do_promote() {
    local target_version="${1:-}"
    if [[ -z "$target_version" ]]; then
        _err "Usage: deploy/upload.sh promote <version>"
        return 1
    fi

    _step "PROMOTE ${target_version} → ${RELEASE_TAG}"

    local artifacts="xli/${target_version}/xli-darwin-arm64
xli/${target_version}/xli-linux-amd64
xli/${target_version}/install.sh"

    local tmp_dir
    tmp_dir="$(mktemp -d)"
    trap "rm -rf '${tmp_dir}'" EXIT

    echo "$artifacts" | while IFS= read -r artifact; do
        local src_url="${ARTIFACTORY_BASE}/${artifact}"
        local filename="${artifact##*/}"
        local dest_url="${ARTIFACTORY_BASE}/xli/${RELEASE_TAG}/${filename}"
        local tmp_file="${tmp_dir}/${filename}"

        _info "Fetching ${artifact}"
        local dl_rc=0
        case "$_AUTH_METHOD" in
            token)  curl -fsSL -H "Authorization: Bearer $_JFROG_TOKEN" -o "$tmp_file" "$src_url" || dl_rc=$? ;;
            netrc)  curl -fsSL -n -o "$tmp_file" "$src_url" || dl_rc=$? ;;
            manual) curl -fsSL -u "$ARTIFACTORY_USER" -o "$tmp_file" "$src_url" || dl_rc=$? ;;
            *)      curl -fsSL -o "$tmp_file" "$src_url" || dl_rc=$? ;;
        esac

        if [[ $dl_rc -ne 0 ]]; then
            _skip "${filename} (not at v${target_version})"
            continue
        fi
        _upload "$tmp_file" "$dest_url"
    done

    _ok "Promoted ${target_version} → ${RELEASE_TAG}"
}

# ══════════════════════════════════════════════════════════════════════
# MAIN
# ══════════════════════════════════════════════════════════════════════
usage() {
    printf "\n%s\n\n" "${_B}${_P}  XLI Upload${_R} ${_D}v${VERSION}${_R}"
    printf "  %s\n\n" "${_B}Usage:${_R} deploy/upload.sh <command>"
    printf "  %s\n"   "${_B}Commands:${_R}"
    printf "    ship              Upload all artifacts (versioned + release)\n"
    printf "    mac               Upload macOS binary only\n"
    printf "    linux             Upload Linux binary only\n"
    printf "    npm               Upload npm tarball only\n"
    printf "    installer         Upload install.sh only\n"
    printf "    promote <ver>     Copy <ver> into the release slot\n\n"
    printf "  %s\n"   "${_B}Flags:${_R}"
    printf "    FORCE=1           Re-upload even if unchanged\n"
    printf "    RELEASE_TAG=beta  Change alias (default: release)\n"
    printf "    VERSION=0.2.0     Override version\n\n"
    printf "  %s\n\n" "${_D}Auth: ${_AUTH_METHOD} │ ${ARTIFACTORY_BASE}/xli/${_R}"
}

case "${1:-}" in
    ship|all)      _do_ship ;;
    mac)           _publish_mac ;;
    linux)         _publish_linux ;;
    npm)           _publish_npm ;;
    installer|install) _publish_installer ;;
    promote)       shift; _do_promote "$@" ;;
    -h|--help|help) usage ;;
    *)             usage ;;
esac

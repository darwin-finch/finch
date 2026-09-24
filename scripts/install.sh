#!/usr/bin/env bash
# finch installer
# Usage: curl -fsSL https://raw.githubusercontent.com/darwin-finch/finch/main/scripts/install.sh | bash
#
# Show where the installer would install without downloading anything:
#   curl -fsSL .../install.sh | bash -s -- --resolve
#
# Destination resolution (#889): FINCH_INSTALL_DIR wins. Otherwise the finch
# that wins PATH resolution (first `which -a finch` hit) is upgraded in place
# when its directory is writable; with no existing install the default
# /usr/local/bin is used. Any further installs stay untouched and are listed.
set -euo pipefail

REPO="darwin-finch/finch"
DEFAULT_DEST="/usr/local/bin"
BOLD=$'\033[1m'; CYAN=$'\033[36m'; GREEN=$'\033[32m'; RED=$'\033[31m'; DIM=$'\033[2m'; RESET=$'\033[0m'

say()  { printf "  %s\n" "$*"; }
ok()   { printf "  ${GREEN}✓${RESET} %s\n" "$*"; }
err()  { printf "  ${RED}✗${RESET} %s\n" "$*" >&2; exit 1; }
head() { printf "\n${BOLD}%s${RESET}\n" "$*"; }

# ── Resolve install destination ────────────────────────────────────────────────
# Sets DEST, DEST_SOURCE, FIRST_HIT, and OTHER_HITS.
path_finch_hits() {
  if command -v which > /dev/null 2>&1; then
    which -a finch 2> /dev/null || true
  fi
}

resolve_dest() {
  FINCH_HITS="$(path_finch_hits)"
  FIRST_HIT=""
  OTHER_HITS=""
  if [[ -n "${FINCH_INSTALL_DIR:-}" ]]; then
    DEST="$FINCH_INSTALL_DIR"
    DEST_SOURCE="FINCH_INSTALL_DIR"
    OTHER_HITS="$FINCH_HITS"
  elif [[ -n "$FINCH_HITS" ]]; then
    FIRST_HIT="${FINCH_HITS%%$'\n'*}"
    DEST="$(dirname "$FIRST_HIT")"
    DEST_SOURCE="existing install on PATH"
    OTHER_HITS="${FINCH_HITS#*$'\n'}"
    if [[ "$OTHER_HITS" == "$FINCH_HITS" ]]; then
      OTHER_HITS=""
    fi
  else
    DEST="$DEFAULT_DEST"
    DEST_SOURCE="default"
  fi
}

# ── Flags ──────────────────────────────────────────────────────────────────────
if [[ "${1:-}" == "--resolve" ]]; then
  resolve_dest
  printf 'dest=%s\n' "$DEST"
  printf 'source=%s\n' "$DEST_SOURCE"
  printf 'hit=%s\n' "${FIRST_HIT:-none}"
  printf 'others=%s\n' "$(printf '%s' "$OTHER_HITS" | tr '\n' ' ' | sed 's/ $//')"
  if [[ -w "$DEST" ]]; then
    printf 'writable=yes\n'
  elif [[ -e "$DEST" ]]; then
    printf 'writable=no\n'
  else
    printf 'writable=missing\n'
  fi
  exit 0
fi

# ── Detect platform ────────────────────────────────────────────────────────────
case "$(uname -sm)" in
  "Darwin arm64") ASSET="finch-macos-arm64.tar.gz" ;;
  "Linux x86_64") ASSET="finch-linux-x86_64.tar.gz" ;;
  "Darwin x86_64") err "Intel Macs are not supported. Use an Apple Silicon Mac (M1/M2/M3/M4) or Linux x86_64." ;;
  *) err "Unsupported platform: $(uname -sm)" ;;
esac

head "installing finch"
say "platform: $(uname -sm)"

# ── Destination ────────────────────────────────────────────────────────────────
resolve_dest

if [[ -n "$FIRST_HIT" ]]; then
  ok "found existing finch at $FIRST_HIT — upgrading in place"
  if [[ -n "$OTHER_HITS" ]]; then
    say "other finch installs on PATH (left untouched): $(printf '%s' "$OTHER_HITS" | tr '\n' ' ' | sed 's/ $//')"
  fi
elif [[ "$DEST_SOURCE" == "FINCH_INSTALL_DIR" && -n "$OTHER_HITS" ]]; then
  say "other finch installs on PATH (left untouched): $(printf '%s' "$OTHER_HITS" | tr '\n' ' ' | sed 's/ $//')"
else
  say "no existing finch found on PATH; installing to $DEST"
fi

# ── Download ───────────────────────────────────────────────────────────────────
URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
say "downloading ${ASSET}…"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if ! curl -fsSL --progress-bar "$URL" -o "$TMP/$ASSET"; then
  err "download failed. Check your internet connection or visit https://github.com/${REPO}/releases"
fi

tar -xz -C "$TMP" -f "$TMP/$ASSET"

# ── Install ────────────────────────────────────────────────────────────────────
if [[ -w "$DEST" ]]; then
  mv "$TMP/finch" "$DEST/finch"
else
  say "need sudo to write to $DEST"
  if ! sudo mv "$TMP/finch" "$DEST/finch"; then
    err "cannot install to $DEST: the directory is not writable and sudo was declined. Fix permissions on $DEST (e.g. sudo chown \"\$(id -un)\" \"$DEST\") or set FINCH_INSTALL_DIR to a writable directory and re-run."
  fi
fi

chmod +x "$DEST/finch"

# ── macOS quarantine ───────────────────────────────────────────────────────────
if [[ "$(uname)" == "Darwin" ]]; then
  xattr -dr com.apple.quarantine "$DEST/finch" 2>/dev/null || true
fi

ok "finch installed → $DEST/finch"
say "version: $("$DEST/finch" --version 2>/dev/null || echo 'unknown')"

# ── Setup ──────────────────────────────────────────────────────────────────────
head "setup"

if [[ -f "$HOME/.finch/config.toml" ]]; then
  ok "config already exists at ~/.finch/config.toml"
  say "run ${CYAN}finch setup${RESET} to reconfigure, or ${CYAN}finch${RESET} to start"
else
  say "No config found. Running setup wizard…"
  say ""
  say "You'll need an API key from one of:"
  say "  ${CYAN}Grok${RESET}    console.x.ai      (free with X Premium+)"
  say "  ${CYAN}Claude${RESET}  console.anthropic.com"
  say "  ${CYAN}GPT-4${RESET}   platform.openai.com"
  say ""
  "$DEST/finch" setup || true
fi

# ── Done ───────────────────────────────────────────────────────────────────────
head "done"
say "start finch:  ${CYAN}finch${RESET}"
say "ask anything in plain English, or just start typing"
say ""
say "  ${DIM}> explain this codebase${RESET}"
say "  ${DIM}> run the tests and tell me what failed${RESET}"
say "  ${DIM}> what changed since last commit${RESET}"
say ""

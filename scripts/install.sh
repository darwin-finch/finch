#!/usr/bin/env bash
# finch installer
# Usage: curl -fsSL https://raw.githubusercontent.com/darwin-finch/finch/main/scripts/install.sh | bash
set -euo pipefail

REPO="darwin-finch/finch"
DEST="${FINCH_INSTALL_DIR:-/usr/local/bin}"
BOLD=$'\033[1m'; CYAN=$'\033[36m'; GREEN=$'\033[32m'; RED=$'\033[31m'; DIM=$'\033[2m'; RESET=$'\033[0m'

say()  { printf "  %s\n" "$*"; }
ok()   { printf "  ${GREEN}✓${RESET} %s\n" "$*"; }
err()  { printf "  ${RED}✗${RESET} %s\n" "$*" >&2; exit 1; }
head() { printf "\n${BOLD}%s${RESET}\n" "$*"; }

# ── Detect platform ────────────────────────────────────────────────────────────
case "$(uname -sm)" in
  "Darwin arm64") ASSET="finch-macos-arm64.tar.gz" ;;
  "Linux x86_64") ASSET="finch-linux-x86_64.tar.gz" ;;
  "Darwin x86_64") err "Intel Macs are not supported. Use an Apple Silicon Mac (M1/M2/M3/M4) or Linux x86_64." ;;
  *) err "Unsupported platform: $(uname -sm)" ;;
esac

head "installing finch"
say "platform: $(uname -sm)"

# ── Download ───────────────────────────────────────────────────────────────────
URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
say "downloading $ASSET…"
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
  sudo mv "$TMP/finch" "$DEST/finch"
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

#!/bin/sh
# MGI-Mind installer for Linux and macOS.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/madgodinc/mgi-mind/main/install.sh | sh
#
# Environment:
#   INSTALL_DIR   target directory for the mgimind binary (default: ~/.local/bin)
#   MGIMIND_TAG   release tag to install (default: latest)
#   SKIP_DOCTOR   set non-empty to skip downloading Qdrant/ONNX/models at the end

set -eu

REPO="madgodinc/mgi-mind"
BIN_NAME="mgimind"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
TAG="${MGIMIND_TAG:-latest}"

say()  { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

# --- requirements ------------------------------------------------------------

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar  >/dev/null 2>&1 || die "tar is required"
command -v uname >/dev/null 2>&1 || die "uname is required"

# --- detect platform ---------------------------------------------------------

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
    Linux)
        case "$arch" in
            x86_64|amd64) target="x86_64-unknown-linux-gnu" ;;
            *) die "unsupported Linux arch: $arch (only x86_64 is published; build from source)" ;;
        esac
        ;;
    Darwin)
        case "$arch" in
            arm64|aarch64) target="aarch64-apple-darwin" ;;
            x86_64)        target="x86_64-apple-darwin" ;;
            *) die "unsupported macOS arch: $arch" ;;
        esac
        ;;
    *)
        die "unsupported OS: $os (Linux/macOS only; Windows users run install.ps1)"
        ;;
esac

say "Detected: $os / $arch -> $target"

# --- pick release URL --------------------------------------------------------

asset="${BIN_NAME}-${target}.tar.gz"
if [ "$TAG" = "latest" ]; then
    url="https://github.com/${REPO}/releases/latest/download/${asset}"
else
    url="https://github.com/${REPO}/releases/download/${TAG}/${asset}"
fi

# --- download + extract ------------------------------------------------------

mkdir -p "$INSTALL_DIR"

tmpdir="$(mktemp -d 2>/dev/null || mktemp -d -t mgimind)"
trap 'rm -rf "$tmpdir"' EXIT INT TERM

say "Downloading $url"
if ! curl -fsSL --proto '=https' --tlsv1.2 -o "$tmpdir/$asset" "$url"; then
    die "download failed (release for $target may not exist yet; check https://github.com/${REPO}/releases)"
fi

# Fetch and verify the SHA-256 checksum published alongside the asset. Fail
# closed: if the .sha256 file is missing OR the hash mismatches, we do not
# install. Pipe-to-shell installs are the canonical place to insist on this.
say "Verifying SHA-256"
if ! curl -fsSL --proto '=https' --tlsv1.2 -o "$tmpdir/$asset.sha256" "$url.sha256"; then
    die "checksum file missing at $url.sha256 — refusing to install unverified binary"
fi
# The .sha256 file is in `shasum -c` format ("<hex>  <name>"); the filename is
# the bare asset name, so rewrite to its tmpdir path before checking.
expected_hex="$(awk '{print $1}' "$tmpdir/$asset.sha256")"
[ -n "$expected_hex" ] || die "checksum file at $url.sha256 is empty or malformed"
printf '%s  %s\n' "$expected_hex" "$tmpdir/$asset" > "$tmpdir/$asset.sha256.local"
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "$tmpdir/$asset.sha256.local" >/dev/null \
        || die "SHA-256 mismatch — refusing to install"
elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "$tmpdir/$asset.sha256.local" >/dev/null \
        || die "SHA-256 mismatch — refusing to install"
else
    die "neither sha256sum nor shasum found — cannot verify download"
fi
say "Checksum OK ($expected_hex)"

say "Extracting to $INSTALL_DIR"
tar -xzf "$tmpdir/$asset" -C "$tmpdir"
[ -f "$tmpdir/$BIN_NAME" ] || die "archive did not contain '$BIN_NAME'"

install -m 0755 "$tmpdir/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME" 2>/dev/null \
    || { cp "$tmpdir/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME" && chmod 0755 "$INSTALL_DIR/$BIN_NAME"; }

# Strip macOS quarantine so the first run does not get blocked by Gatekeeper.
if [ "$os" = "Darwin" ] && command -v xattr >/dev/null 2>&1; then
    xattr -d com.apple.quarantine "$INSTALL_DIR/$BIN_NAME" 2>/dev/null || true
fi

bin_path="$INSTALL_DIR/$BIN_NAME"
say "Installed: $bin_path"

# --- PATH sanity check -------------------------------------------------------

case ":$PATH:" in
    *":$INSTALL_DIR:"*) on_path=yes ;;
    *) on_path=no ;;
esac

if [ "$on_path" = "no" ]; then
    warn "$INSTALL_DIR is not on PATH. Add this to your shell profile:"
    printf '    export PATH="%s:$PATH"\n' "$INSTALL_DIR" >&2
fi

# --- init + doctor (download Qdrant, ONNX, models) ---------------------------

if [ -n "${SKIP_DOCTOR:-}" ]; then
    say "SKIP_DOCTOR set; skipping data-dir setup. Run '$bin_path doctor --fix' yourself."
else
    say ""
    say "Setting up data directory and downloading runtime + models (~600 MB)..."
    "$bin_path" init       || die "'mgimind init' failed"
    "$bin_path" doctor --fix || die "'mgimind doctor --fix' failed"
fi

# --- final message -----------------------------------------------------------

cat <<EOF

Done. To wire mgi-mind into Claude Code, run:

    claude mcp add mgimind -- $bin_path mcp

(Other MCP clients: point them at '$bin_path mcp' over stdio.)

See AI_INSTRUCTIONS.md in the repo for the assistant-side protocol.
EOF

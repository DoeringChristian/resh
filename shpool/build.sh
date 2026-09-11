#!/usr/bin/env bash
set -eu

# Build shpool from source for the current platform and store it in the resh
# bin directory. Run this on each target platform to populate the binary cache.
#
# On Linux, builds a statically-linked musl binary for maximum portability.
# Requires: cargo (and musl-tools on Linux for static linking)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN_DIR="$SCRIPT_DIR/bin"
mkdir -p "$BIN_DIR"

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

# Normalize arch names
case "$ARCH" in
    amd64) ARCH="x86_64" ;;
    arm64) ARCH="aarch64" ;;
esac

TARGET="shpool-${OS}-${ARCH}"

if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo is required to build shpool" >&2
    exit 1
fi

# Pinned upstream ref: builds are reproducible and the patches in
# shpool/patches/ are written against exactly this tree.
SHPOOL_REF="${SHPOOL_REF:-v0.11.4}"
PATCH_DIR="$SCRIPT_DIR/patches"

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

echo "Cloning shpool $SHPOOL_REF..."
git clone --depth 1 --branch "$SHPOOL_REF" https://github.com/shell-pool/shpool.git "$TMPDIR/shpool"
cd "$TMPDIR/shpool"

# Apply local patches. A failure means upstream moved and the patch needs
# rebasing -- fail loudly rather than silently shipping an unpatched binary.
if [ -d "$PATCH_DIR" ]; then
    for p in "$PATCH_DIR"/*.patch; do
        [ -e "$p" ] || continue
        echo "Applying $(basename "$p")..."
        git apply "$p" || {
            echo "Error: $(basename "$p") does not apply to $SHPOOL_REF" >&2
            exit 1
        }
    done
fi

if [ "$OS" = "linux" ]; then
    # Build statically with musl for portability across Linux distros
    MUSL_TARGET="${ARCH}-unknown-linux-musl"
    echo "Building shpool for ${MUSL_TARGET} (static)..."
    rustup target add "$MUSL_TARGET" 2>/dev/null || true
    cargo build --release --target "$MUSL_TARGET"
    cp "target/$MUSL_TARGET/release/shpool" "$BIN_DIR/$TARGET"
else
    echo "Building shpool for ${OS}-${ARCH}..."
    cargo build --release
    cp target/release/shpool "$BIN_DIR/$TARGET"
fi

chmod +x "$BIN_DIR/$TARGET"

# Record what this binary actually is -- upstream ref, commit and patches.
{
    echo "ref:      $SHPOOL_REF"
    echo "commit:   $(git rev-parse HEAD)"
    echo "version:  $(grep -m1 '^version' libshpool/Cargo.toml | cut -d'"' -f2)"
    printf "patches: "
    if [ -d "$PATCH_DIR" ]; then for p in "$PATCH_DIR"/*.patch; do [ -e "$p" ] && printf " %s" "$(basename "$p")"; done; fi
    echo
    echo "built:    $(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$BIN_DIR/VERSION"

echo "Built: $BIN_DIR/$TARGET"
cat "$BIN_DIR/VERSION"
ls -lh "$BIN_DIR/$TARGET"
file "$BIN_DIR/$TARGET"

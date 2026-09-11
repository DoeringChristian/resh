shpool_repo := "https://github.com/shell-pool/shpool.git"
# Pinned upstream ref. Builds are reproducible and the patches in
# shpool/patches/ are written against exactly this tree.
shpool_ref := "v0.11.4"
shpool_dir := "shpool/bin"
shpool_patches := "shpool/patches"

# Build sshr
build:
    cargo build --release

# Build shpool for all supported targets and place in shpool/bin/
shpool-all: (shpool "x86_64-unknown-linux-musl" "linux-x86_64") (shpool "aarch64-unknown-linux-musl" "linux-aarch64") (shpool-native "darwin-aarch64")

# Build shpool for a cross-compilation target (requires cargo-zigbuild and zig)
shpool rust_target name:
    #!/usr/bin/env bash
    set -eu
    out="{{shpool_dir}}/shpool-{{name}}"
    if [ -f "$out" ]; then
        echo "shpool-{{name}} already exists, skipping (use 'just shpool-force' to rebuild all)"
        exit 0
    fi
    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT
    echo "Building shpool for {{name}} ({{rust_target}})..."
    git clone --depth 1 --branch {{shpool_ref}} {{shpool_repo}} "$tmpdir/shpool"
    cd "$tmpdir/shpool"
    just --justfile "{{justfile()}}" _apply-shpool-patches "$tmpdir/shpool"
    rustup target add {{rust_target}} 2>/dev/null || true
    cargo zigbuild --release --target {{rust_target}}
    mkdir -p "{{justfile_directory()}}/{{shpool_dir}}"
    cp "target/{{rust_target}}/release/shpool" "{{justfile_directory()}}/$out"
    chmod +x "{{justfile_directory()}}/$out"
    just --justfile "{{justfile()}}" _stamp-shpool-version "$tmpdir/shpool"
    echo "Built: $out"

# Build shpool natively (for current platform, e.g. macOS)
shpool-native name:
    #!/usr/bin/env bash
    set -eu
    out="{{shpool_dir}}/shpool-{{name}}"
    if [ -f "$out" ]; then
        echo "shpool-{{name}} already exists, skipping (use 'just shpool-force' to rebuild all)"
        exit 0
    fi
    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT
    echo "Building shpool for {{name}} (native)..."
    git clone --depth 1 --branch {{shpool_ref}} {{shpool_repo}} "$tmpdir/shpool"
    cd "$tmpdir/shpool"
    just --justfile "{{justfile()}}" _apply-shpool-patches "$tmpdir/shpool"
    cargo build --release
    mkdir -p "{{justfile_directory()}}/{{shpool_dir}}"
    cp "target/release/shpool" "{{justfile_directory()}}/$out"
    chmod +x "{{justfile_directory()}}/$out"
    just --justfile "{{justfile()}}" _stamp-shpool-version "$tmpdir/shpool"
    echo "Built: $out"

# Force rebuild all shpool binaries
shpool-force:
    rm -f {{shpool_dir}}/shpool-*
    just shpool-all

# Apply every patch in shpool/patches/ to a fresh shpool checkout. Patches are
# written against `shpool_ref`; a failure here means upstream moved and the
# patch needs rebasing -- which is the moment you want to find out, not later.
_apply-shpool-patches checkout:
    #!/usr/bin/env bash
    set -eu
    patches="{{justfile_directory()}}/{{shpool_patches}}"
    [ -d "$patches" ] || exit 0
    shopt -s nullglob
    for p in "$patches"/*.patch; do
        echo "  applying $(basename "$p")"
        git -C "{{checkout}}" apply --verbose "$p" 2>/dev/null || {
            echo "error: $(basename "$p") does not apply to {{shpool_ref}}" >&2
            exit 1
        }
    done

# Record what the bundled binaries actually are: upstream ref, commit, patches.
_stamp-shpool-version checkout:
    #!/usr/bin/env bash
    set -eu
    shopt -s nullglob
    patches=""
    for p in "{{justfile_directory()}}/{{shpool_patches}}"/*.patch; do
        patches="$patches $(basename "$p")"
    done
    {
        echo "ref:      {{shpool_ref}}"
        echo "commit:   $(git -C "{{checkout}}" rev-parse HEAD)"
        echo "version:  $(grep -m1 '^version' "{{checkout}}/libshpool/Cargo.toml" | cut -d'"' -f2)"
        echo "patches: ${patches:- (none)}"
        echo "built:    $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    } > "{{justfile_directory()}}/{{shpool_dir}}/VERSION"
    cat "{{justfile_directory()}}/{{shpool_dir}}/VERSION"

# Clean all build artifacts
clean:
    cargo clean
    rm -f {{shpool_dir}}/shpool-*

# Smoke-test sshr against an ephemeral Linux container (debian + sshd, no shpool, no fish)
smoke: build
    #!/usr/bin/env bash
    set -euo pipefail
    name=sshr-smoke
    port=2222
    platform=linux/amd64
    binary=shpool-linux-x86_64

    rt=${SSHR_CONTAINER_RUNTIME:-}
    if [ -z "$rt" ]; then
        if command -v docker >/dev/null 2>&1; then rt=docker
        elif command -v podman >/dev/null 2>&1; then rt=podman
        else echo "error: need docker or podman on PATH" >&2; exit 1
        fi
    fi

    if [ ! -f "{{shpool_dir}}/$binary" ]; then
        echo "error: {{shpool_dir}}/$binary not found — run 'just shpool-all' first" >&2
        exit 1
    fi

    keydir=$(mktemp -d)
    cleanup() {
        rm -rf "$keydir"
        rm -f "$HOME/.ssh/sshr-sockets/sshr@127.0.0.1:$port"
        "$rt" rm -f "$name" >/dev/null 2>&1 || true
    }
    trap cleanup EXIT

    echo "==> using runtime: $rt"
    echo "==> generating ephemeral key"
    ssh-keygen -t ed25519 -N '' -f "$keydir/id" -q

    echo "==> building test image ($platform)"
    "$rt" build --platform "$platform" -q -t "$name" -f test/Dockerfile test/ >/dev/null

    echo "==> starting container on 127.0.0.1:$port"
    "$rt" run -d --rm --platform "$platform" --name "$name" \
        -p "127.0.0.1:$port:22" "$name" >/dev/null
    "$rt" cp "$keydir/id.pub" "$name:/home/sshr/.ssh/authorized_keys"
    "$rt" exec "$name" chown sshr:sshr /home/sshr/.ssh/authorized_keys
    "$rt" exec "$name" chmod 600 /home/sshr/.ssh/authorized_keys

    echo "==> waiting for sshd"
    for _ in $(seq 1 30); do
        ssh -q -i "$keydir/id" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
            -p "$port" sshr@127.0.0.1 true 2>/dev/null && break
        sleep 0.5
    done

    echo "==> launching sshr -v (exit shell to tear down)"
    ./target/release/sshr -v sshr@127.0.0.1 \
        -i "$keydir/id" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -p "$port"

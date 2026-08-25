#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)
TARGET=${TARGET:-x86_64-unknown-linux-gnu}
PROFILE=${PROFILE:-release}

if [[ "$PROFILE" != "release" ]]; then
    echo "Only the release profile is supported by this packaging script" >&2
    exit 2
fi

if ! rustup target list --installed | grep -Fxq "$TARGET"; then
    echo "Rust target $TARGET is not installed" >&2
    echo "Install it with: rustup target add $TARGET" >&2
    exit 2
fi

if ! command -v sha256sum >/dev/null 2>&1; then
    echo "sha256sum is required to create release checksums" >&2
    exit 2
fi

VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -n 1)
if [[ -z "$VERSION" ]]; then
    echo "Could not read package version from Cargo.toml" >&2
    exit 2
fi

ARCH=$(case "$TARGET" in
    x86_64-unknown-linux-gnu) printf '%s' x86_64 ;;
    aarch64-unknown-linux-gnu) printf '%s' aarch64 ;;
    *) printf '%s' "$TARGET" | tr '/' '-' ;;
esac)
PACKAGE="kiwix-cli-${VERSION}-linux-${ARCH}"
DIST_DIR="$ROOT_DIR/dist"
STAGE_DIR="$DIST_DIR/$PACKAGE"
BINARY="$ROOT_DIR/target/$TARGET/$PROFILE/kiwix-cli"
ARCHIVE="$DIST_DIR/$PACKAGE.tar.gz"
CHECKSUM="$ARCHIVE.sha256"

rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR"

cd "$ROOT_DIR"
cargo test --locked --all-targets --all-features
cargo build --locked --release --target "$TARGET"

if [[ ! -x "$BINARY" ]]; then
    echo "Expected Linux binary was not produced: $BINARY" >&2
    exit 1
fi

cp "$BINARY" "$STAGE_DIR/kiwix-cli"
cp LICENSE README.md README.zh-CN.md "$STAGE_DIR/"
if command -v strip >/dev/null 2>&1; then
    strip "$STAGE_DIR/kiwix-cli"
fi

tar -C "$DIST_DIR" -czf "$ARCHIVE" "$PACKAGE"
(cd "$DIST_DIR" && sha256sum "$(basename "$ARCHIVE")" > "$(basename "$CHECKSUM")")
rm -rf "$STAGE_DIR"

printf 'Built Linux release:\n  %s\n  %s\n' "$ARCHIVE" "$CHECKSUM"

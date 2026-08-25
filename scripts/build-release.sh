#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)
PROFILE=${PROFILE:-release}
HOST_OS=$(uname -s)

if [[ "$PROFILE" != "release" ]]; then
    echo "Only the release profile is supported by this packaging script" >&2
    exit 2
fi

if [[ -z "${PLATFORM:-}" ]]; then
    case "$HOST_OS" in
        Linux) PLATFORM=linux ;;
        Darwin) PLATFORM=macos ;;
        *) echo "Unsupported build host: $HOST_OS" >&2; exit 2 ;;
    esac
fi

case "$PLATFORM" in
    linux)
        [[ "$HOST_OS" == "Linux" ]] || { echo "Linux packages must be built on a Linux host" >&2; exit 2; }
        TARGET=${TARGET:-x86_64-unknown-linux-gnu}
        ;;
    macos)
        [[ "$HOST_OS" == "Darwin" ]] || { echo "macOS packages must be built on a macOS host with the Apple SDK" >&2; exit 2; }
        case "$(uname -m)" in
            arm64) DEFAULT_TARGET=aarch64-apple-darwin ;;
            x86_64) DEFAULT_TARGET=x86_64-apple-darwin ;;
            *) echo "Unsupported macOS architecture: $(uname -m)" >&2; exit 2 ;;
        esac
        TARGET=${TARGET:-$DEFAULT_TARGET}
        ;;
    *) echo "PLATFORM must be linux or macos" >&2; exit 2 ;;
esac

if ! rustup target list --installed | grep -Fxq "$TARGET"; then
    echo "Rust target $TARGET is not installed" >&2
    echo "Install it with: rustup target add $TARGET" >&2
    exit 2
fi

VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -n 1)
if [[ -z "$VERSION" ]]; then
    echo "Could not read package version from Cargo.toml" >&2
    exit 2
fi

ARCH=$(case "$TARGET" in
    x86_64-unknown-linux-gnu|x86_64-apple-darwin) printf '%s' x86_64 ;;
    aarch64-unknown-linux-gnu) printf '%s' aarch64 ;;
    aarch64-apple-darwin) printf '%s' arm64 ;;
    *) printf '%s' "$TARGET" | tr '/' '-' ;;
esac)
PACKAGE="kiwix-cli-${VERSION}-${PLATFORM}-${ARCH}"
DIST_DIR="$ROOT_DIR/dist"
STAGE_DIR="$DIST_DIR/$PACKAGE"
BINARY="$ROOT_DIR/target/$TARGET/$PROFILE/kiwix-cli"
ARCHIVE="$DIST_DIR/$PACKAGE.tar.gz"
CHECKSUM="$ARCHIVE.sha256"

checksum_archive() {
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$DIST_DIR" && sha256sum "$(basename "$ARCHIVE")" > "$(basename "$CHECKSUM")")
    elif command -v shasum >/dev/null 2>&1; then
        (cd "$DIST_DIR" && shasum -a 256 "$(basename "$ARCHIVE")" > "$(basename "$CHECKSUM")")
    else
        echo "sha256sum or shasum is required to create release checksums" >&2
        exit 2
    fi
}

rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR"

cd "$ROOT_DIR"
cargo test --locked --all-targets --all-features
cargo build --locked --release --target "$TARGET"

if [[ ! -x "$BINARY" ]]; then
    echo "Expected $PLATFORM binary was not produced: $BINARY" >&2
    exit 1
fi

cp "$BINARY" "$STAGE_DIR/kiwix-cli"
cp LICENSE README.md README.zh-CN.md "$STAGE_DIR/"
mkdir -p "$STAGE_DIR/man/man1"
cp man/kiwix-cli.1 "$STAGE_DIR/man/man1/"
if command -v strip >/dev/null 2>&1; then
    strip "$STAGE_DIR/kiwix-cli"
fi
if [[ "$PLATFORM" == "macos" ]]; then
    codesign --force --sign - "$STAGE_DIR/kiwix-cli"
fi

tar -C "$DIST_DIR" -czf "$ARCHIVE" "$PACKAGE"
checksum_archive
rm -rf "$STAGE_DIR"

printf 'Built %s release:\n  %s\n  %s\n' "$PLATFORM" "$ARCHIVE" "$CHECKSUM"

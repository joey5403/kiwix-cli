#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
export PLATFORM=macos
exec "$SCRIPT_DIR/build-release.sh"

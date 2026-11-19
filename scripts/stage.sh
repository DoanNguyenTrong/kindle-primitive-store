#!/bin/sh
# Stage the Store files after building the Kindle binary with cargo zigbuild.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
ARCH=${1:-armhf}
[ "$ARCH" = armhf ] || { echo "Only armhf is packaged for now." >&2; exit 1; }
BIN=${CARGO_TARGET_DIR:-target}/armv7-unknown-linux-musleabihf/release/store-app
[ -x "$BIN" ] || {
    echo "Build the Store binary with cargo zigbuild --release -p store-app --target armv7-unknown-linux-musleabihf first." >&2
    exit 1
}

STAGE=build/stage
rm -rf "$STAGE"
mkdir -p "$STAGE/store/bin" "$STAGE/documents"
install -m 755 "$BIN" "$STAGE/store/bin/kl-store-app"
install -m 755 deploy/Store.sh "$STAGE/documents/Store.sh"
echo "Staged $STAGE"

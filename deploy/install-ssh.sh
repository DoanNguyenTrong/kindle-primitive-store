#!/bin/sh
# Install Store on the Kindle over SSH (Wi-Fi), on its own: its program and its library entry. It does not
# touch the launcher. The store's catalog, installed apps and records stay in /mnt/us/launcher/store.
#
#   deploy/install-ssh.sh path/to/kl-store-app
#
# Environment: KINDLE_HOST (default 192.168.1.28), KINDLE_SSH_PORT (default 2222). The root password is asked once.
set -eu

HERE=$(cd "$(dirname "$0")/.." && pwd)
BIN=${1:?usage: install-ssh.sh path/to/kl-store-app}
HOST=${KINDLE_HOST:-192.168.1.28}
PORT=${KINDLE_SSH_PORT:-2222}
[ -f "$BIN" ] || { echo "binary not found: $BIN" >&2; exit 1; }
file "$BIN" | grep -q ARM || { echo "not an ARM binary: $BIN" >&2; exit 1; }

CTL="/tmp/klstore-ssh-$$"
SSH="ssh -p $PORT -o ControlMaster=auto -o ControlPath=$CTL -o ControlPersist=120 -o StrictHostKeyChecking=accept-new root@$HOST"
cleanup() { ssh -p "$PORT" -o ControlPath="$CTL" -O exit "root@$HOST" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

$SSH 'mkdir -p /mnt/us/store/bin'
local_sum=$(shasum -a 256 "$BIN" | cut -c1-64)
remote_sum=$($SSH 'sha256sum /mnt/us/store/bin/kl-store-app 2>/dev/null' | cut -c1-64 || true)
if [ "$local_sum" != "$remote_sum" ]; then
    $SSH "cat > /mnt/us/store/bin/kl-store-app.new && sync && mv /mnt/us/store/bin/kl-store-app.new /mnt/us/store/bin/kl-store-app && chmod 755 /mnt/us/store/bin/kl-store-app" < "$BIN"
    echo "copied: /mnt/us/store/bin/kl-store-app"
fi
$SSH "cat > /mnt/us/documents/Store.sh.new && sync && mv /mnt/us/documents/Store.sh.new /mnt/us/documents/Store.sh && chmod 755 /mnt/us/documents/Store.sh" < "$HERE/deploy/Store.sh"
remote_sum=$($SSH 'sha256sum /mnt/us/store/bin/kl-store-app' | cut -c1-64)
if [ "$local_sum" != "$remote_sum" ]; then
    echo "checksum mismatch" >&2
    exit 1
fi
echo "installed Store ($(wc -c < "$BIN" | tr -d ' ') bytes, sha256 $local_sum)"

#!/bin/sh
# Name: Store
# Author: Primitive AI
# DontUseFBInk
# Wake: redraw
# Opens Store from the Kindle library. The program is copied to tmpfs and run from there, so /mnt/us stays free
# for USB drive mode. When the store ends (Done), the launcher takes the screen again.

# Under the launcher SIGUSR1 asks Store to redraw: this script ignores it (a shell ends on it otherwise), and Store
# sets its own handler.
[ "${PRIMITIVE_LAUNCHER:-0}" = 1 ] && trap '' USR1

BIN=/mnt/us/store/bin/kl-store-app
RUN=/var/tmp/kl-store

# Use the kernel clock so launch timing needs no child process and survives wall clock changes.
read -r launch_started _ < /proc/uptime || launch_started=
[ -x "$BIN" ] || exit 1
mkdir -p "$RUN"
cp "$BIN" "$RUN/kl-store-app" && chmod 755 "$RUN/kl-store-app" || exit 1
if [ -n "$launch_started" ] && read -r launch_ready _ < /proc/uptime; then
    printf '[kl] store: wrapper started at %ss and ready at %ss uptime\n' "$launch_started" "$launch_ready" >&2
fi
cd / && exec "$RUN/kl-store-app"

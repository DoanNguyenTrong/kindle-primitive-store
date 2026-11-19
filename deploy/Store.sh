#!/bin/sh
# Name: Primitive Store
# Author: Primitive AI
# DontUseFBInk
# Opens Primitive Store from the Kindle library. The program is copied to tmpfs and run from there, so /mnt/us stays free
# for USB drive mode. When the store ends (Done), the launcher takes the screen again.

BIN=/mnt/us/store/bin/kl-store-app
RUN=/var/tmp/kl-store

[ -x "$BIN" ] || exit 1
mkdir -p "$RUN"
cp "$BIN" "$RUN/kl-store-app" && chmod 755 "$RUN/kl-store-app" || exit 1
cd / && exec "$RUN/kl-store-app"

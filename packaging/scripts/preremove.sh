#!/bin/sh
# Undoes the postinstall `systemctl --global enable` on removal (not on
# upgrade). Running daemons stop at logout.
set -e

case "$1" in
    remove|0)  # deb: remove; rpm: last version removed
        if command -v systemctl >/dev/null 2>&1; then
            systemctl --global disable tuxdropd.service || true
        fi
        ;;
esac

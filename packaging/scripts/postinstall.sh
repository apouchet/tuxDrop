#!/bin/sh
# Starts tuxdropd in every user's graphical session from their next login.
# Runs on each install and upgrade (enabling twice changes nothing); to opt
# out durably, mask instead of disabling: `systemctl --global mask tuxdropd`
# for everyone, `systemctl --user mask tuxdropd` for one user.
set -e

if command -v systemctl >/dev/null 2>&1; then
    systemctl --global enable tuxdropd.service || true
fi

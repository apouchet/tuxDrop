#!/usr/bin/env bash
# Installs tuxDrop for the current user: the app in /Applications (or
# ~/Applications) with its Share menu entry, started at each login, and the
# tuxdrop command.
#   scripts/install-macos.sh            build, install and start
#   scripts/install-macos.sh uninstall  stop and remove (keeps your identity
#                                       and settings in ~/Library/Application Support/tuxdrop)
set -euo pipefail
cd "$(dirname "$0")/.."

LABEL=org.tuxdrop.tuxdropd
AGENT=~/Library/LaunchAgents/$LABEL.plist
DOMAIN=gui/$(id -u)
SHARE=org.tuxdrop.tuxDrop.share

stop() {
    launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
    pkill -x tuxdropd 2>/dev/null || true
}

cli_dirs=(/opt/homebrew/bin /usr/local/bin ~/.local/bin)

if [[ "${1:-}" == uninstall ]]; then
    stop
    rm -f "$AGENT"
    for app in /Applications/tuxDrop.app ~/Applications/tuxDrop.app; do
        if [[ -d "$app/Contents/PlugIns" ]]; then
            pluginkit -r "$app/Contents/PlugIns/tuxDropShare.appex" 2>/dev/null || true
        fi
    done
    for dir in "${cli_dirs[@]}"; do
        [[ -L "$dir/tuxdrop" ]] && rm -f "$dir/tuxdrop"
    done
    rm -rf /Applications/tuxDrop.app ~/Applications/tuxDrop.app
    echo "tuxDrop removed"
    exit 0
fi

scripts/package-macos.sh

DEST=/Applications
[[ -w "$DEST" ]] || DEST=~/Applications
mkdir -p "$DEST"
stop
rm -rf "$DEST/tuxDrop.app"
cp -R dist/tuxDrop.app "$DEST/"

mkdir -p ~/Library/LaunchAgents ~/Library/Logs
sed -e "s#@APP@#$DEST/tuxDrop.app#" -e "s#@LOG@#$HOME/Library/Logs/tuxdrop.log#" \
    packaging/macos/$LABEL.plist > "$AGENT"
plutil -lint "$AGENT" >/dev/null
launchctl bootstrap "$DOMAIN" "$AGENT"

# Share menu entry: register the extension now rather than at the next
# login, and enable it.
EXT="$DEST/tuxDrop.app/Contents/PlugIns/tuxDropShare.appex"
if [[ -d "$EXT" ]]; then
    /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$DEST/tuxDrop.app"
    pluginkit -a "$EXT"
    pluginkit -e use -i "$SHARE" || true
fi

linked=
for dir in "${cli_dirs[@]}"; do
    if [[ -d "$dir" && -w "$dir" ]]; then
        ln -sf "$DEST/tuxDrop.app/Contents/MacOS/tuxdrop" "$dir/tuxdrop"
        linked=$dir
        break
    fi
done

echo "tuxDrop installed in $DEST and started; it starts at each login."
if [[ -n "$linked" ]]; then
    echo "Command: $linked/tuxdrop"
else
    echo "Command: sudo ln -s '$DEST/tuxDrop.app/Contents/MacOS/tuxdrop' /usr/local/bin/tuxdrop"
fi
if [[ -d "$EXT" ]]; then
    echo "Finder: right-click files > Share > tuxDrop"
fi
echo "Logs: ~/Library/Logs/tuxdrop.log"

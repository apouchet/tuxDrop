#!/usr/bin/env bash
# Builds dist/tuxDrop.app (tuxdropd as the app, the tuxdrop CLI next to it,
# the Share menu extension in PlugIns), then dist/tuxDrop-<version>-macos-<arch>.dmg
# (drag the app to Applications) and a .zip of the app.
#
# The icon and the Share menu extension need Xcode or the Command Line Tools
# (xcode-select --install). Optional settings:
#   UNIVERSAL=1                 arm64 + x86_64 binary (rustup target add
#                               x86_64-apple-darwin aarch64-apple-darwin)
#   CODESIGN_IDENTITY="Developer ID Application: …"
#                               sign for distribution (hardened runtime);
#                               default: ad-hoc, fine on this Mac only
#   NOTARY_PROFILE=<profile>    then notarize the disk image (profile saved
#                               with `xcrun notarytool store-credentials`)
set -euo pipefail
cd "$(dirname "$0")/.."

IDENTITY=${CODESIGN_IDENTITY:--}
sign() {
    if [[ "$IDENTITY" == - ]]; then
        codesign --force --sign - "$@"
    else
        codesign --force --sign "$IDENTITY" --options runtime --timestamp "$@"
    fi
}
if [[ -n "${NOTARY_PROFILE:-}" && "$IDENTITY" == - ]]; then
    echo "NOTARY_PROFILE needs CODESIGN_IDENTITY: Apple does not notarize ad-hoc signatures" >&2
    exit 1
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
APP=dist/tuxDrop.app

if [[ "${UNIVERSAL:-0}" == 1 ]]; then
    ARCH=universal
    SWIFT_ARCHS=(arm64 x86_64)
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
        cargo build --release --locked --target "$target"
    done
    bin() {
        lipo -create -output "$2" \
            "target/aarch64-apple-darwin/release/$1" "target/x86_64-apple-darwin/release/$1"
    }
else
    ARCH=$(uname -m)
    SWIFT_ARCHS=("$ARCH")
    cargo build --release --locked
    bin() { cp "target/release/$1" "$2"; }
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null
bin tuxdropd "$APP/Contents/MacOS/tuxdropd"
bin tuxdrop "$APP/Contents/MacOS/tuxdrop"

# Signatures (at least ad-hoc): required on Apple silicon, and they give the
# app a stable identity for the Local Network permission. Nested code first.
sign --identifier org.tuxdrop.tuxdrop "$APP/Contents/MacOS/tuxdrop"

if xcrun --find swiftc >/dev/null 2>&1; then
    BUILD=target/macos-bundle
    rm -rf "$BUILD"
    mkdir -p "$BUILD" "$APP/Contents/Resources"
    xcrun swiftc -O packaging/macos/make-icon.swift -o "$BUILD/make-icon"
    "$BUILD/make-icon" "$BUILD/tuxDrop.iconset"
    iconutil -c icns "$BUILD/tuxDrop.iconset" -o "$APP/Contents/Resources/tuxDrop.icns"

    # Share menu extension (Finder, Preview…).
    EXT="$APP/Contents/PlugIns/tuxDropShare.appex"
    mkdir -p "$EXT/Contents/MacOS" "$EXT/Contents/Resources"
    sed "s/@VERSION@/$VERSION/g" packaging/macos/share/Info.plist > "$EXT/Contents/Info.plist"
    plutil -lint "$EXT/Contents/Info.plist" >/dev/null
    cp "$APP/Contents/Resources/tuxDrop.icns" "$EXT/Contents/Resources/"
    slices=()
    for arch in "${SWIFT_ARCHS[@]}"; do
        xcrun swiftc -O -swift-version 5 -target "$arch-apple-macos11.0" \
            -application-extension -parse-as-library -module-name tuxDropShare \
            -Xlinker -e -Xlinker _NSExtensionMain \
            packaging/macos/share/ShareViewController.swift -o "$BUILD/tuxDropShare-$arch"
        slices+=("$BUILD/tuxDropShare-$arch")
    done
    lipo -create -output "$EXT/Contents/MacOS/tuxDropShare" "${slices[@]}"
    sign --entitlements packaging/macos/share/share.entitlements "$EXT"
else
    echo "warning: no Swift compiler (xcode-select --install): no icon, no Share menu entry" >&2
fi

sign --identifier org.tuxdrop.tuxDrop "$APP"
codesign --verify --strict "$APP"

NAME="tuxDrop-$VERSION-macos-$ARCH"

# Disk image: the app and a link to Applications to drop it on.
DMG="dist/$NAME.dmg"
STAGE=target/macos-dmg
rm -rf "$STAGE" "$DMG"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/tuxDrop.app"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname tuxDrop -srcfolder "$STAGE" -fs HFS+ -format UDZO "$DMG"
if [[ "$IDENTITY" != - ]]; then
    codesign --force --sign "$IDENTITY" --timestamp "$DMG"
fi
if [[ -n "${NOTARY_PROFILE:-}" ]]; then
    xcrun notarytool submit "$DMG" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$DMG"
fi

ZIP="dist/$NAME.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
ls -l "$DMG" "$ZIP"

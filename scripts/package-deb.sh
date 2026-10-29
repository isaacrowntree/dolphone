#!/usr/bin/env bash
# Build dolphone_<version>_arm64.deb: the daemon, the CLI, the desktop app, the
# panel bundles, and their system integration. Flipper OS is Debian, so this is
# how Dolphone installs there:
#
#     scripts/package-deb.sh                 # target/deb/dolphone_<ver>_arm64.deb
#     sudo apt install ./target/deb/dolphone_*_arm64.deb
#     dolphone-install-apps                   # as the user: panel apps into ~/Apps/Phone
#
# Runs on aarch64 (the dev machine, a board, CI's arm runner).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$ROOT/target}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
ARCH=arm64
PKG=$ROOT/target/deb/dolphone_${VERSION}_${ARCH}
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

echo "== binaries =="
cargo build -q --release --locked --manifest-path "$ROOT/Cargo.toml" -p phoned -p phonectl -p dolphone-desktop
[ -n "${SKIP_BUNDLES:-}" ] || "$ROOT/scripts/bundle.sh" | grep -E "^== [0-9.]+[KM]" || true

rm -rf "$PKG"
d() { mkdir -p "$PKG/$1"; }
d DEBIAN
d usr/bin
d usr/lib/systemd/user
d usr/share/dbus-1/services
d usr/share/applications
d usr/share/icons/hicolor/scalable/apps
d usr/share/metainfo
d usr/share/polkit-1/rules.d
d usr/share/pipewire/pipewire.conf.d
d usr/share/wireplumber/wireplumber.conf.d
d usr/share/dolphone/apps
d "usr/share/doc/dolphone"

install -m 0755 "$CARGO_TARGET_DIR"/release/{phoned,phonectl,dolphone-desktop} "$PKG/usr/bin/"
install -m 0644 "$ROOT/dist/systemd/phoned.service" "$PKG/usr/lib/systemd/user/"
install -m 0644 "$ROOT/dist/dbus-1/io.github.isaacrowntree.Dolphone.service" "$PKG/usr/share/dbus-1/services/"
install -m 0644 "$ROOT/dist/desktop/io.github.isaacrowntree.Dolphone.desktop" "$PKG/usr/share/applications/"
install -m 0644 "$ROOT/dist/desktop/io.github.isaacrowntree.Dolphone.svg" "$PKG/usr/share/icons/hicolor/scalable/apps/"
install -m 0644 "$ROOT/dist/desktop/io.github.isaacrowntree.Dolphone.metainfo.xml" "$PKG/usr/share/metainfo/"
install -m 0644 "$ROOT/dist/polkit/50-dolphone.rules" "$PKG/usr/share/polkit-1/rules.d/"
install -m 0644 "$ROOT/dist/pipewire/dolphone-echo-cancel.conf" "$PKG/usr/share/pipewire/pipewire.conf.d/"
install -m 0644 "$ROOT/dist/wireplumber/51-dolphone-bluetooth.conf" "$PKG/usr/share/wireplumber/wireplumber.conf.d/"
install -m 0644 "$ROOT/dist/audio.example.json" "$PKG/usr/share/doc/dolphone/"
for b in "$ROOT"/target/bundles/dolphone-*-aarch64.fap.AppImage; do
    [ -f "$b" ] && install -m 0755 "$b" "$PKG/usr/share/dolphone/apps/"
done
cat > "$PKG/usr/bin/dolphone-install-apps" <<'EOF'
#!/bin/sh
# Put Dolphone's panel apps in this user's Apps folder (Apps > Phone in FlipCTL).
# Apps are per user on Flipper OS: ~/Apps survives a factory reset and every
# OS profile shares it.
set -e
mkdir -p "$HOME/Apps/Phone"
cp /usr/share/dolphone/apps/dolphone-*-aarch64.fap.AppImage "$HOME/Apps/Phone/"
echo "installed to $HOME/Apps/Phone: Phone, Messages, Phone Settings"
EOF
chmod 0755 "$PKG/usr/bin/dolphone-install-apps"
cp "$ROOT/LICENSE" "$PKG/usr/share/doc/dolphone/copyright"
cp -r "$ROOT/LICENSES" "$PKG/usr/share/doc/dolphone/LICENSES"
[ -f "$ROOT/README.md" ] && cp "$ROOT/README.md" "$PKG/usr/share/doc/dolphone/"

size=$(du -sk "$PKG/usr" | cut -f1)
# GNOME Calls and Chatty are ModemManager voice and messaging clients too: beside
# phoned they would race it for every call and text (and Chatty deletes texts from
# the SIM itself), so the package refuses to sit next to them.
cat > "$PKG/DEBIAN/control" <<EOF
Package: Dolphone
Version: $VERSION
Architecture: $ARCH
Maintainer: Isaac Rowntree <isaacrowntree@users.noreply.github.com>
Installed-Size: $size
Depends: modemmanager (>= 1.20), pipewire (>= 1.0), pipewire-bin, wireplumber, libfontconfig1, libxkbcommon0
Recommends: bluez, pipewire-pulse, alsa-utils
Conflicts: gnome-calls, chatty
Suggests: baresip
Section: comm
Priority: optional
Homepage: https://github.com/isaacrowntree/dolphone
Description: calls and texts for the Flipper One
 Dolphone turns a Flipper One with a cellular modem into a phone: VoLTE calls,
 texts, contacts and the call log, Bluetooth headsets and the 3.5 mm jack.
 phoned owns calls, texts and call audio; FlipCTL apps put it on the panel and
 dolphone-desktop on an external display. Unofficial: not by Flipper Devices.
 Not suitable for emergency calls.
EOF
cat > "$PKG/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
    # Start phoned in every user session from the next login; D-Bus activation
    # starts it sooner if anything asks.
    systemctl --global enable phoned.service >/dev/null 2>&1 || true
    command -v update-desktop-database >/dev/null && update-desktop-database -q /usr/share/applications || true
    command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q /usr/share/icons/hicolor || true
    echo "Dolphone: run dolphone-install-apps as your user to add the panel apps to ~/Apps/Phone."
fi
EOF
cat > "$PKG/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = remove ]; then
    systemctl --global disable phoned.service >/dev/null 2>&1 || true
fi
EOF
chmod 0755 "$PKG/DEBIAN/postinst" "$PKG/DEBIAN/prerm"

dpkg-deb --root-owner-group --build "$PKG" >/dev/null
echo "== $(du -h "$PKG.deb" | cut -f1) $PKG.deb =="

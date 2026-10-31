#!/usr/bin/env bash
# Build dolphone_<version>_<arch>.deb: the daemon, the CLI, the desktop app, the
# device profiles and their system integration, and on arm64 the Flipper One's
# panel apps. Flipper OS is Debian, so this is how Dolphone installs there:
#
#     scripts/package-deb.sh                 # target/deb/dolphone_<ver>_<arch>.deb
#     sudo apt install ./target/deb/dolphone_*.deb
#     dolphone-install-apps                   # as the user: panel apps into ~/Apps/Phone
#
# Builds for the machine it runs on: arm64 (the Flipper One, a Raspberry Pi, a
# PinePhone) or amd64 (a laptop), which gets everything but the panel apps.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$ROOT/target}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
ARCH=$(dpkg --print-architecture)
PKG=$ROOT/target/deb/dolphone_${VERSION}_${ARCH}
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

echo "== binaries =="
cargo build -q --release --locked --manifest-path "$ROOT/Cargo.toml" -p phoned -p phonectl -p dolphone-desktop -p dolphone-gateway
# The panel apps are FlipCTL bundles, which exist for the Flipper One's arm64.
if [ "$ARCH" = arm64 ] && [ -z "${SKIP_BUNDLES:-}" ]; then
    "$ROOT/scripts/bundle.sh" | grep -E "^== [0-9.]+[KM]" || true
fi

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
d usr/share/polkit-1/actions
d usr/share/pipewire/pipewire.conf.d
d usr/share/wireplumber/wireplumber.conf.d
d usr/share/dolphone/apps
d usr/share/dolphone/devices
d usr/lib/udev/rules.d
d "usr/share/doc/dolphone"

install -m 0755 "$CARGO_TARGET_DIR"/release/{phoned,phonectl,dolphone-desktop,dolphone-gateway} "$PKG/usr/bin/"
install -m 0644 "$ROOT/dist/systemd/phoned.service" "$PKG/usr/lib/systemd/user/"
# Installed, not enabled: systemctl --user enable --now dolphone-gateway
install -m 0644 "$ROOT/dist/systemd/dolphone-gateway.service" "$PKG/usr/lib/systemd/user/"
install -m 0644 "$ROOT/dist/dbus-1/io.github.isaacrowntree.Dolphone.service" "$PKG/usr/share/dbus-1/services/"
install -m 0644 "$ROOT/dist/desktop/io.github.isaacrowntree.Dolphone.desktop" "$PKG/usr/share/applications/"
install -m 0644 "$ROOT/dist/desktop/io.github.isaacrowntree.Dolphone.svg" "$PKG/usr/share/icons/hicolor/scalable/apps/"
install -m 0644 "$ROOT/dist/desktop/io.github.isaacrowntree.Dolphone.metainfo.xml" "$PKG/usr/share/metainfo/"
install -m 0644 "$ROOT/dist/polkit/50-dolphone.rules" "$PKG/usr/share/polkit-1/rules.d/"
install -m 0644 "$ROOT/dist/pipewire/dolphone-echo-cancel.conf" "$PKG/usr/share/pipewire/pipewire.conf.d/"
install -m 0644 "$ROOT/dist/wireplumber/51-dolphone-bluetooth.conf" "$PKG/usr/share/wireplumber/wireplumber.conf.d/"
install -m 0644 "$ROOT/dist/audio.example.json" "$PKG/usr/share/doc/dolphone/"
# ModemManager's debug mode, for Dolphone's AT commands: the install asks
# (dist/debian/templates), and dolphone-modem-access turns it on or off later.
d usr/share/dolphone/modemmanager
install -m 0644 "$ROOT/dist/modemmanager/dolphone-debug.conf" "$PKG/usr/share/dolphone/modemmanager/"
install -m 0755 "$ROOT/dist/bin/dolphone-modem-access" "$PKG/usr/bin/"
install -m 0644 "$ROOT/dist/polkit/io.github.isaacrowntree.Dolphone.policy" "$PKG/usr/share/polkit-1/actions/"
install -m 0644 "$ROOT"/dist/devices/*.toml "$PKG/usr/share/dolphone/devices/"
# How an AI agent uses phonectl (docs/AGENTS.md).
d usr/share/dolphone/skills/phonectl
install -m 0644 "$ROOT/dist/skills/phonectl/SKILL.md" "$PKG/usr/share/dolphone/skills/phonectl/"
# The spare AT port rule does nothing until dolphone-modem-access port on.
install -m 0644 "$ROOT/dist/udev/70-dolphone-modem-wakeup.rules" "$ROOT/dist/udev/70-dolphone-leds.rules" \
    "$ROOT/dist/udev/70-dolphone-at-port.rules" "$PKG/usr/lib/udev/rules.d/"
for b in "$ROOT"/target/bundles/dolphone-*-aarch64.fap.AppImage; do
    [ -f "$b" ] && install -m 0755 "$b" "$PKG/usr/share/dolphone/apps/"
done
[ "$ARCH" = arm64 ] && cat > "$PKG/usr/bin/dolphone-install-apps" <<'EOF'
#!/bin/sh
# Put Dolphone's panel apps in this user's Apps folder (Apps > Phone in FlipCTL).
# Apps are per user on Flipper OS: ~/Apps survives a factory reset and every
# OS profile shares it.
set -e
mkdir -p "$HOME/Apps/Phone"
cp /usr/share/dolphone/apps/dolphone-*-aarch64.fap.AppImage "$HOME/Apps/Phone/"
echo "installed to $HOME/Apps/Phone: Phone, Messages, Phone Settings"
EOF
[ "$ARCH" = arm64 ] && chmod 0755 "$PKG/usr/bin/dolphone-install-apps"
# What is under which licence, and where the source of the GPL binaries is.
{
    cat <<COPYRIGHT
Dolphone $VERSION
Source: https://github.com/isaacrowntree/dolphone

Dolphone's own source is MIT (below). dolphone-desktop, and the panel apps in
/usr/share/dolphone/apps, link the Slint toolkit under its GPL-3.0-only option, so
those binaries are GPL-3.0-only (LICENSES/GPL-3.0-only.txt); their complete
corresponding source is attached to this version's release on GitHub, as
dolphone-$VERSION-source.tar.zst, and is in the repository at this version's tag.
phoned, phonectl and dolphone-gateway are MIT; their dependencies' notices are in
THIRD-PARTY-LICENSES.md.

COPYRIGHT
    cat "$ROOT/LICENSE"
} >"$PKG/usr/share/doc/dolphone/copyright"
cp -r "$ROOT/LICENSES" "$PKG/usr/share/doc/dolphone/LICENSES"
# Attribution for every crate in the daemon, CLI and desktop app (SQLCipher among
# them); scripts/gen-third-party-licenses.sh keeps it in step with Cargo.lock.
cp "$ROOT/THIRD-PARTY-LICENSES.md" "$PKG/usr/share/doc/dolphone/"
[ -f "$ROOT/README.md" ] && cp "$ROOT/README.md" "$PKG/usr/share/doc/dolphone/"

size=$(du -sk "$PKG/usr" | cut -f1)
# GNOME Calls and Chatty are ModemManager voice and messaging clients too: beside
# phoned they would race it for every call and text (and Chatty deletes texts from
# the SIM itself), so the package refuses to sit next to them.
cat > "$PKG/DEBIAN/control" <<EOF
Package: dolphone
Version: $VERSION
Architecture: $ARCH
Maintainer: Isaac Rowntree <isaacrowntree@users.noreply.github.com>
Installed-Size: $size
Depends: debconf (>= 0.5) | debconf-2.0, adduser, polkitd, modemmanager (>= 1.20), pipewire (>= 1.0), pipewire-bin, wireplumber, libfontconfig1, libxkbcommon0, libssl3t64
Recommends: pkexec, mmsd-tng, bluez, bluez-obexd, pipewire-pulse, alsa-utils, baresip-core, wl-clipboard | xclip
Suggests: espeak-ng
Conflicts: gnome-calls, chatty
Section: comm
Priority: optional
Homepage: https://github.com/isaacrowntree/dolphone
Description: calls and texts for the Flipper One
 Dolphone turns a Flipper One with a cellular modem into a phone: calls (VoLTE,
 and SIP over the internet), texts, emergency alerts, contacts and the call log,
 Bluetooth headsets and the 3.5 mm jack. phoned owns calls, texts and call
 audio; FlipCTL apps put it on the panel and dolphone-desktop on an external
 display. Device profiles carry it to other Linux machines with a modem (a
 Raspberry Pi, a uConsole, a PinePhone, a laptop) through the desktop app.
 Unofficial: not by Flipper Devices. Not suitable for emergency calls.
EOF
install -m 0644 "$ROOT/dist/debian/templates" "$PKG/DEBIAN/templates"
install -m 0755 "$ROOT/dist/debian/config" "$PKG/DEBIAN/config"
cat > "$PKG/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
. /usr/share/debconf/confmodule
if [ "$1" = configure ]; then
    # ModemManager's debug mode, as answered (DEBIAN/templates).
    db_get dolphone/modem-access || RET=false
    access=$RET
    # Done with debconf: from here stdout is the terminal again.
    db_stop
    if [ "$access" = true ]; then
        dolphone-modem-access on >/dev/null || echo "Dolphone: could not turn on ModemManager's debug mode; run sudo dolphone-modem-access on" >&2
    else
        dolphone-modem-access off >/dev/null || true
    fi
    # Start phoned in every user session from the next login (D-Bus activation
    # starts it sooner if anything asks). Only on a first install: an admin who
    # disabled it keeps that through upgrades.
    if [ -z "$2" ]; then
        systemctl --global enable phoned.service >/dev/null 2>&1 || true
    fi
    command -v update-desktop-database >/dev/null && update-desktop-database -q /usr/share/applications || true
    command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q /usr/share/icons/hicolor || true
    # The group whose members may flash the LEDs (70-dolphone-leds.rules) and use
    # the modem's spare AT port (70-dolphone-at-port.rules).
    getent group dolphone >/dev/null || addgroup --system dolphone >/dev/null
    if command -v udevadm >/dev/null; then
        udevadm control --reload-rules >/dev/null 2>&1 || true
        udevadm trigger --subsystem-match=leds --action=change >/dev/null 2>&1 || true
    fi
    if [ -x /usr/bin/dolphone-install-apps ]; then
        echo "Dolphone: run dolphone-install-apps as your user to add the panel apps to ~/Apps/Phone."
    fi
    if [ -n "$SUDO_USER" ] && ! id -nG "$SUDO_USER" | grep -qw dolphone; then
        echo "Dolphone: to flash the LEDs for a ringing call (and use a spare AT port), sudo adduser $SUDO_USER dolphone, then log in again."
    fi
fi
EOF
cat > "$PKG/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = remove ]; then
    systemctl --global disable phoned.service >/dev/null 2>&1 || true
    # Leave ModemManager as it was before Dolphone: debug mode off, and every
    # port its own.
    dolphone-modem-access off >/dev/null 2>&1 || true
    dolphone-modem-access port off >/dev/null 2>&1 || true
fi
EOF
cat > "$PKG/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = remove ] && command -v udevadm >/dev/null; then
    # The LED rule is gone: let udev forget it (the LEDs' group stays until reboot).
    udevadm control --reload-rules >/dev/null 2>&1 || true
fi
if [ "$1" = purge ] && [ -e /usr/share/debconf/confmodule ]; then
    . /usr/share/debconf/confmodule
    db_purge
fi
EOF
chmod 0755 "$PKG/DEBIAN/postinst" "$PKG/DEBIAN/prerm" "$PKG/DEBIAN/postrm"

dpkg-deb --root-owner-group --build "$PKG" >/dev/null
echo "== $(du -h "$PKG.deb" | cut -f1) $PKG.deb =="

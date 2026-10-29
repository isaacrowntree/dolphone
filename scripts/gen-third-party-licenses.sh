#!/bin/sh
# Regenerate THIRD-PARTY-LICENSES.md for everything Dolphone ships: each panel app
# (its bundle carries the file), and the daemon, CLI and desktop app (the Debian
# package carries theirs). Run whenever dependencies change.
#
#     cargo install cargo-about --features cli
#     scripts/gen-third-party-licenses.sh
#
# --fail stops on any crate whose license is not in about.toml's accepted list, so a
# new dependency cannot ship unattributed. about.toml and about.hbs are adapted from
# flipctl-slint's, since the panel apps link the same framework.
set -eu
cd "$(dirname "$0")/.."
for app in apps/dialer apps/messages apps/settings; do
    cargo about generate --fail --manifest-path "$app/Cargo.toml" -c about.toml about.hbs -o "$app/THIRD-PARTY-LICENSES.md"
    echo "wrote $app/THIRD-PARTY-LICENSES.md"
done
cargo about generate --fail --workspace -c about.toml about.hbs -o THIRD-PARTY-LICENSES.md
echo "wrote THIRD-PARTY-LICENSES.md"

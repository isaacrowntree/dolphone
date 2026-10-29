#!/usr/bin/env bash
# The corresponding source for a release's binaries: this tree plus every Rust
# dependency vendored, including the exact flipctl-slint commit the panel apps were
# built against. The panel binaries link Slint under GPL-3.0-only, which obliges us
# to offer this; it is attached to every release.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=$(git -C "$ROOT" describe --tags --always --dirty)
OUT=$ROOT/target/source
WORK=$(mktemp -d)
mkdir -p "$OUT"
git -C "$ROOT" archive --format=tar --prefix="dolphone-$VERSION/" HEAD | tar -x -C "$WORK"
cd "$WORK/dolphone-$VERSION"
mkdir -p .cargo
cargo vendor --locked vendor > .cargo/config.toml
# Each panel app is a workspace of its own, with its own vendored crates; the
# config cargo vendor prints goes beside it, so it builds offline from here too
# (the root .cargo/config.toml would otherwise point it at the root's vendor/,
# which lacks flipctl-slint).
for app in apps/dialer apps/messages apps/settings; do
    mkdir -p "$app/.cargo"
    (cd "$app" && cargo vendor --locked ../../vendor-"$(basename "$app")" > .cargo/config.toml)
done
# What the bundles were packed with and carry from flipctl-slint: its AppImage
# tools (the scripts, and the lock naming the pinned runtime) and the panel fonts.
FLIPCTL=${FLIPCTL:-$HOME/flipctl-slint}
if [ -d "$FLIPCTL/tools/appimage" ]; then
    mkdir -p third_party/flipctl-slint
    cp -r "$FLIPCTL/tools/appimage" third_party/flipctl-slint/tools-appimage
    cp -r "$FLIPCTL/third_party/flipctl-fonts" third_party/flipctl-slint/flipctl-fonts
    rm -rf third_party/flipctl-slint/tools-appimage/__pycache__
fi
cd "$WORK"
tar --zstd -cf "$OUT/dolphone-$VERSION-source.tar.zst" "dolphone-$VERSION"
echo "== $(du -h "$OUT/dolphone-$VERSION-source.tar.zst" | cut -f1) $OUT/dolphone-$VERSION-source.tar.zst =="

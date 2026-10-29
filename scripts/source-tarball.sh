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
for app in apps/dialer apps/messages apps/settings; do
    (cd "$app" && cargo vendor --locked ../../vendor-"$(basename "$app")" > /dev/null)
done
cd "$WORK"
tar --zstd -cf "$OUT/dolphone-$VERSION-source.tar.zst" "dolphone-$VERSION"
echo "== $(du -h "$OUT/dolphone-$VERSION-source.tar.zst" | cut -f1) $OUT/dolphone-$VERSION-source.tar.zst =="

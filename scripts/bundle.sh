#!/usr/bin/env bash
# Build the panel apps and pack each one as a FlipCTL bundle
# (dolphone-<app>-aarch64.fap.AppImage), with upstream's own staging and pinned
# AppImage tools, so a bundle is laid out exactly as flipctl's first-party apps are.
#
#     scripts/bundle.sh                  # all three, into target/bundles
#     scripts/bundle.sh dialer           # one
#     INSTALL=1 scripts/bundle.sh        # and copy them to ~/Apps/Phone
#
# Runs on an aarch64 Linux host (the dev machine, a board, or CI's arm runner).
# FLIPCTL points at a flipctl-slint checkout whose tools/appimage is used; its
# pinned tools are fetched by building any upstream app once with
# `tools/appimage/build.sh --native apps/sysmon`, which scripts/dev-setup.sh does.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
FLIPCTL=${FLIPCTL:-$HOME/flipctl-slint}
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}/apps
OUT=$ROOT/target/bundles
TOOLS=$FLIPCTL/target/appimage/tools
APPS=("$@")
[ ${#APPS[@]} -gt 0 ] || APPS=(dialer messages settings)

[ "$(uname -m)" = aarch64 ] || { echo "bundle.sh: build on aarch64 (the panel's architecture)" >&2; exit 1; }
[ -x "$TOOLS/appimagetool-aarch64" ] || { echo "bundle.sh: no pinned tools in $TOOLS; run scripts/dev-setup.sh" >&2; exit 1; }

version=$(git -C "$ROOT" describe --always --dirty 2>/dev/null || echo 0.1.0)
upstream=$(git -C "$FLIPCTL" rev-parse --short HEAD 2>/dev/null || echo unknown)

# What bundle.py copies licenses from: our LICENSE and texts, and upstream's fonts,
# whose glyphs every app carries.
repo=$OUT/.repo
rm -rf "$repo"
mkdir -p "$repo/third_party"
cp "$ROOT/LICENSE" "$repo/LICENSE"
cp -r "$ROOT/LICENSES" "$repo/LICENSES"
cp -r "$FLIPCTL/third_party/flipctl-fonts" "$repo/third_party/flipctl-fonts"

mkdir -p "$OUT"
for app in "${APPS[@]}"; do
    src=$ROOT/apps/$app
    id=dolphone-$app
    echo "== building $id (flipctl-slint $upstream) =="
    # The build's own status, not the filter's: a failed build must not leave the
    # last good binary to be bundled as if it were this one.
    set +e
    CARGO_TARGET_DIR=$TARGET cargo build --release --locked --manifest-path "$src/Cargo.toml" 2>&1 \
        | grep -vE "^\s+(Compiling|Downloaded|Downloading)"
    built=${PIPESTATUS[0]}
    set -e
    [ "$built" = 0 ] || { echo "building $id failed" >&2; exit 1; }
    bin=$TARGET/release/$id
    [ -x "$bin" ] || { echo "no binary at $bin" >&2; exit 1; }
    # Upstream's link-surface check (tools/appimage/build.sh): a framework app needs
    # libc, libm and libgcc_s and nothing else; more is a C dependency creeping in,
    # which the panel's base image may not have.
    needed=$(readelf -d "$bin" | awk '/NEEDED/ { gsub(/[][]/, "", $5); print $5 }' | sort | tr '\n' ' ')
    [ "$needed" = "libc.so.6 libgcc_s.so.1 libm.so.6 " ] || {
        echo "$id links more than libc, libm and libgcc_s: $needed" >&2
        exit 1
    }

    # Staged under the name phoned looks for, so `flipctl open` finds it by file.
    stage=$OUT/.stage/$id
    rm -rf "$stage"
    mkdir -p "$stage"
    cp "$src/app.toml" "$src/icon.png" "$src/NOTICE.md" "$src/Cargo.toml" "$stage/"
    [ -f "$src/THIRD-PARTY-LICENSES.md" ] && cp "$src/THIRD-PARTY-LICENSES.md" "$stage/"
    python3 "$FLIPCTL/tools/appimage/bundle.py" stage "$stage" "$OUT/$id/AppDir" \
        --binary "$bin" --version "$version" --repo "$repo"

    echo "== packing $id-aarch64.fap.AppImage =="
    rm -f "$OUT/$id-aarch64.fap.AppImage"
    # Packed in /tmp: appimagetool reads its output back through a shared mmap,
    # which a virtiofs share (the dev machine's view of the Mac) refuses.
    packed=$(mktemp -d)/$id-aarch64.fap.AppImage
    SOURCE_DATE_EPOCH=$(git -C "$ROOT" log -1 --format=%ct 2>/dev/null || date +%s) \
        "$TOOLS/appimagetool-aarch64" --appimage-extract-and-run -n --comp zstd \
        --mksquashfs-opt -Xcompression-level --mksquashfs-opt 19 \
        --runtime-file "$TOOLS/runtime-aarch64" \
        "$OUT/$id/AppDir" "$packed" 2>&1 | grep -E "^(Success|Error)" || true
    cp "$packed" "$OUT/"
    (cd "$OUT" && sha256sum "$id-aarch64.fap.AppImage" > "$id-aarch64.fap.AppImage.sha256")
    echo "== $(du -h "$OUT/$id-aarch64.fap.AppImage" | cut -f1) $OUT/$id-aarch64.fap.AppImage =="

    if [ -n "${INSTALL:-}" ]; then
        mkdir -p "$HOME/Apps/Phone"
        cp "$OUT/$id-aarch64.fap.AppImage" "$HOME/Apps/Phone/"
    fi
done

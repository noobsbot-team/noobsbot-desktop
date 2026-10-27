#!/bin/sh
# Builds xwayland-satellite (X11 apps, E6.9) at install.sh's XWS_REV with our patches from vendor/xwayland-satellite,
# in <dir> (a checkout it makes, or reuses): the program is <dir>/target/release/xwayland-satellite. It ships beside
# the desktop's programs, which start it from there.
# Usage: sh scripts/build-xwayland-satellite.sh <dir>
set -eu
repo=$(cd "$(dirname "$0")/.." && pwd)
rev=$(sed -n 's/^XWS_REV=//p' "$repo/install.sh")
dir=$1
mkdir -p "$dir"
git -C "$dir" init -q
git -C "$dir" cat-file -e "$rev^{commit}" 2>/dev/null ||
    git -C "$dir" fetch -q --depth 1 https://github.com/Supreeeme/xwayland-satellite "$rev"
# -f drops the patches of an earlier run, so they apply again cleanly.
git -C "$dir" checkout -qf --detach "$rev"
git -C "$dir" apply "$repo"/vendor/xwayland-satellite/*.patch
cargo build --release --locked --manifest-path "$dir/Cargo.toml" --target-dir "$dir/target"

#!/bin/sh
# Installs this checkout to try it in the real session: a release build that keeps its symbols and line tables (so
# `addr2line -fCi -e <the installed binary> <offset>` reads a frozen loop's backtrace in session.log), as version
# <Cargo.toml's>+<commit>, made the current one as install.sh's use_version does. Applies at the next login, and the
# session wrapper's rollback still covers it. Builds as you, installs with sudo.
# Usage: sh scripts/dev-install.sh
set -eu
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR=target/dev CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
cargo build --workspace --release --locked
lib=/usr/lib/noobsbot-desktop
v=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)+$(git rev-parse --short HEAD)$(git diff --quiet HEAD || echo .dirty)
for b in noobsbot-desktop noobsbot-desktop-settings noobsbot-desktop-update noobsbot-desktop-watchdog noobsbot-desktop-widget-system; do
    sudo install -Dm755 "target/dev/release/$b" "$lib/versions/$v/$b"
done
sudo ln -sfn "$lib/versions/$v/noobsbot-desktop" /usr/bin/noobsbot-desktop
sudo ln -sfn "$lib/versions/$v/noobsbot-desktop-settings" /usr/bin/noobsbot-desktop-settings
sudo install -Dm755 "$lib/versions/$v/noobsbot-desktop-update" "$lib/noobsbot-desktop-update"
echo "installed $v: log out and back in to run it"

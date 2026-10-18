#!/bin/sh
# Runs IN the VM (as tester): turns the install.sh install into an older one so Software Update has something to do.
# /tmp holds a build whose Cargo.toml says 0.1.0 (the updater is newer than the v0.1.0 release).
set -eu
lib=/usr/lib/noobsbot-desktop
sudo rm -rf "$lib/versions/0.1.0" "$lib/versions/"[0-9]*
sudo mkdir -p "$lib/versions/0.1.0"
for b in noobsbot-desktop noobsbot-desktop-settings noobsbot-desktop-update; do
    sudo install -m755 "/tmp/$b" "$lib/versions/0.1.0/$b"
done
sudo install -m755 /tmp/noobsbot-desktop-update "$lib/noobsbot-desktop-update"
sudo ln -sfn "$lib/versions/0.1.0/noobsbot-desktop" /usr/bin/noobsbot-desktop
sudo ln -sfn "$lib/versions/0.1.0/noobsbot-desktop-settings" /usr/bin/noobsbot-desktop-settings
rm -rf ~/.cache/noobsbot-desktop/updates ~/.local/state/noobsbot-desktop
# The worktree's session wrapper, when copied along (it keeps the log in ~/.local/state/noobsbot-desktop/session.log).
[ ! -f /tmp/noobsbot-desktop-session ] || sudo install -m755 /tmp/noobsbot-desktop-session /usr/bin/noobsbot-desktop-session
noobsbot-desktop --version
ls -la /usr/bin/noobsbot-desktop $lib/versions

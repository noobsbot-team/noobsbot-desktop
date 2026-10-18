#!/bin/sh
# Checks install.sh without touching the machine: stages its files twice in a temporary DESTDIR and HOME,
# then checks the result and that the second run changes nothing. Usage: sh scripts/test-install.sh
set -eu
repo=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export DESTDIR="$tmp/root" HOME="$tmp/home"
unset XDG_CONFIG_HOME XDG_CACHE_HOME
cfg="$DESTDIR$HOME/.config" # user files are staged under DESTDIR too
# What the greetd package ships, so the keyring lines have something to go into.
mkdir -p "$DESTDIR/etc/pam.d" "$cfg"
printf '#%%PAM-1.0\n\nauth      include   system-login\nsession   include   system-login\n' >"$DESTDIR/etc/pam.d/greetd"
# The owner's own browser flag and terminal list survive.
echo --enable-features=Foo >"$cfg/helium-browser-flags.conf"
echo kitty.desktop >"$cfg/xdg-terminals.list"

"$repo/install.sh" >"$tmp/first.log" 2>&1 || { cat "$tmp/first.log"; exit 1; }
before=$(cd "$tmp" && find root -type f -printf '%p %T@ %m\n' | sort)
"$repo/install.sh" >"$tmp/second.log" 2>&1 || { cat "$tmp/second.log"; exit 1; }
after=$(cd "$tmp" && find root -type f -printf '%p %T@ %m\n' | sort)

fail() { echo "FAIL: $* (log: $tmp)"; exit 1; }
[ "$before" = "$after" ] || fail "the second run rewrote files"
[ -x "$DESTDIR/usr/bin/noobsbot-desktop-session" ] || fail "session wrapper not executable"
for f in etc/greetd/config.toml etc/pam.d/noobsbot-desktop usr/share/xdg-desktop-portal/noobsbot-desktop-portals.conf \
    usr/share/wayland-sessions/noobsbot-desktop.desktop usr/lib/systemd/user/noobsbot-desktop-session.target \
    etc/UPower/UPower.conf.d/50-noobsbot-desktop.conf; do
    [ -f "$DESTDIR/$f" ] || fail "missing /$f"
done
[ "$(grep -c pam_gnome_keyring "$DESTDIR/etc/pam.d/greetd")" = 2 ] || fail "keyring PAM lines"
[ "$(head -1 "$cfg/xdg-terminals.list")" = noobsbot.desktop ] || fail "terminal list order"
grep -qx kitty.desktop "$cfg/xdg-terminals.list" || fail "owner's terminal entry lost"
grep -qx -- --enable-features=Foo "$cfg/helium-browser-flags.conf" || fail "owner's browser flag lost"
grep -qx -- --password-store=gnome-libsecret "$cfg/helium-browser-flags.conf" || fail "browser flags"
grep -qx TerminalApplication=noobsbot "$cfg/kdeglobals" || fail "kdeglobals"
grep -qx Enabled=false "$cfg/kwalletrc" || fail "kwalletrc"
cmp -s "$repo/data/config.toml" "$cfg/noobsbot-desktop/config.toml" || fail "default config"
echo "install.sh: ok"

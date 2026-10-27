#!/bin/sh
# Checks install.sh without touching the machine: stages its files twice in a temporary DESTDIR and HOME, for Arch and
# for Debian, then checks the result and that the second run changes nothing. Usage: sh scripts/test-install.sh
set -eu
repo=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
unset XDG_CONFIG_HOME XDG_CACHE_HOME

fail() { echo "FAIL ($distro): $* (log: $tmp/$distro)"; exit 1; }

for distro in arch debian; do
    t=$tmp/$distro
    export DESTDIR="$t/root" HOME="$t/home" NOOBSBOT_DISTRO=$distro
    cfg="$DESTDIR$HOME/.config" # user files are staged under DESTDIR too
    # What the greetd package ships, so the keyring lines have something to go into.
    mkdir -p "$DESTDIR/etc/pam.d" "$cfg"
    printf '#%%PAM-1.0\n\nauth      include   system-login\nsession   include   system-login\n' >"$DESTDIR/etc/pam.d/greetd"
    # The owner's own browser flag and terminal list survive.
    echo --enable-features=Foo >"$cfg/helium-browser-flags.conf"
    echo kitty.desktop >"$cfg/xdg-terminals.list"
    # Debian's Helium package's menu entry, for the helium-browser one.
    if [ $distro = debian ]; then
        mkdir -p "$DESTDIR/usr/share/applications"
        printf '[Desktop Entry]\nName=Helium\nExec=helium %%U\n\n[Desktop Action new-window]\nExec=helium\n' \
            >"$DESTDIR/usr/share/applications/helium.desktop"
    fi

    "$repo/install.sh" >"$t.first.log" 2>&1 || { cat "$t.first.log"; exit 1; }
    before=$(cd "$t" && find root -type f -printf '%p %T@ %m\n' | sort)
    "$repo/install.sh" >"$t.second.log" 2>&1 || { cat "$t.second.log"; exit 1; }
    after=$(cd "$t" && find root -type f -printf '%p %T@ %m\n' | sort)

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

    if [ $distro = arch ]; then
        grep -q system-auth "$DESTDIR/etc/pam.d/noobsbot-desktop" || fail "Arch PAM file"
        grep -qx 'user = "greeter"' "$DESTDIR/etc/greetd/config.toml" || fail "Arch greetd user"
        [ ! -e "$DESTDIR/etc/xdg/menus/noobsbot-applications.menu" ] || fail "Debian menu on Arch"
        [ ! -e "$DESTDIR/usr/local/bin/helium-browser" ] || fail "Debian Helium wrapper on Arch"
    else
        grep -qx '@include common-auth' "$DESTDIR/etc/pam.d/noobsbot-desktop" || fail "Debian PAM file"
        grep -qx 'user = "_greetd"' "$DESTDIR/etc/greetd/config.toml" || fail "Debian greetd user"
        grep -qx 'vt = 7' "$DESTDIR/etc/greetd/config.toml" || fail "Debian greetd VT"
        [ -f "$DESTDIR/etc/xdg/menus/noobsbot-applications.menu" ] || fail "Debian app menu"
        [ -x "$DESTDIR/usr/local/bin/helium-browser" ] || fail "Helium wrapper"
        entry=$DESTDIR/usr/local/share/applications/helium.desktop
        [ "$(grep -c '^Exec=helium-browser' "$entry")" = 2 ] || fail "Helium menu entry"
        ! grep -q '^Exec=helium\( \|$\)' "$entry" || fail "Helium menu entry still runs helium"
    fi
    echo "install.sh ($distro): ok"
done

# The release install.sh takes while there is no stable one yet (1.0.0's pre-releases): the newest by SemVer
# precedence, whatever GitHub's order; once there is a stable one, only stable ones.
newest() { printf '%s\n' "$@" | bash -c "$(sed -n '/^newest_tag()/,/^}/p' "$repo/install.sh"); newest_tag"; }
[ "$(newest v1.0.0-beta.2 v1.0.0-beta.10 v1.0.0-alpha.8)" = v1.0.0-beta.10 ] || { echo "FAIL: beta.10 is newer than beta.2"; exit 1; }
[ "$(newest v1.0.0-beta.10 v1.0.0-rc.1)" = v1.0.0-rc.1 ] || { echo "FAIL: rc.1 is newer than beta.10"; exit 1; }
[ "$(newest v1.1.0-beta.1 v1.0.0 v1.0.0-rc.1)" = v1.0.0 ] || { echo "FAIL: a stable release wins"; exit 1; }
echo "install.sh (release pick): ok"

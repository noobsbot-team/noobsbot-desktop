#!/bin/sh
# Checks the session wrapper's update rollback and install at login (E6.13) without touching the session: systemctl,
# gdbus, dbus-update-activation-environment and pkexec are stubs, and the versions are fake compositors.
# Usage: sh scripts/test-session.sh
set -eu
repo=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/lib/versions/1.0.0-beta.10" "$tmp/lib/versions/1.1.0-rc.1" "$tmp/lib/versions/1.2.0"
for stub in systemctl gdbus dbus-update-activation-environment pkexec; do
    printf '#!/bin/sh\necho "%s $*" >>"%s/calls"\n' "$stub" "$tmp" >"$tmp/bin/$stub"
done
# pkexec "installs" 1.2.0, as the helper would.
printf 'ln -sfn "%s/lib/versions/1.2.0/noobsbot-desktop" "%s/bin/noobsbot-desktop"\n' "$tmp" "$tmp" >>"$tmp/bin/pkexec"
# 1.0.0-beta.10 runs past the boot-ok marker; 1.1.0-rc.1 quits at once. Each runs the `--` command, like the compositor.
# Pre-releases (1.0.0's are): the wrapper compares version dirs only by name, never orders them.
printf '#!/bin/sh\necho 1.0.0-beta.10${NOOBSBOT_HEARTBEAT_FD:+ beats} >>"%s/ran"\nshift; "$@"; sleep 1.5\n' "$tmp" >"$tmp/lib/versions/1.0.0-beta.10/noobsbot-desktop"
printf '#!/bin/sh\necho 1.1.0-rc.1 >>"%s/ran"\nexit 134\n' "$tmp" >"$tmp/lib/versions/1.1.0-rc.1/noobsbot-desktop"
printf '#!/bin/sh\necho 1.2.0 >>"%s/ran"\nshift; "$@"\n' "$tmp" >"$tmp/lib/versions/1.2.0/noobsbot-desktop"
chmod +x "$tmp"/bin/* "$tmp"/lib/versions/*/noobsbot-desktop
export PATH="$tmp/bin:$PATH" NOOBSBOT_LIB="$tmp/lib" XDG_STATE_HOME="$tmp/state" NOOBSBOT_BOOT_OK_AFTER=1
session() { sh "$repo/data/system/usr/bin/noobsbot-desktop-session" 2>>"$tmp/log" || true; }
fail() { echo "FAIL: $*"; cat "$tmp/ran" "$tmp/log"; exit 1; }

ln -sfn "$tmp/lib/versions/1.0.0-beta.10/noobsbot-desktop" "$tmp/bin/noobsbot-desktop"
session
[ "$(cat "$tmp/state/noobsbot-desktop/boot-ok")" = 1.0.0-beta.10 ] || fail "1.0.0-beta.10 not marked good"

# The update to 1.1.0-rc.1: three crashes, then 1.0.0-beta.10 again, and it stays there (with a notification).
ln -sfn "$tmp/lib/versions/1.1.0-rc.1/noobsbot-desktop" "$tmp/bin/noobsbot-desktop"
: >"$tmp/ran"
session; session; session; session; session
[ "$(tr '\n' ' ' <"$tmp/ran")" = "1.1.0-rc.1 1.1.0-rc.1 1.1.0-rc.1 1.0.0-beta.10 1.0.0-beta.10 " ] || fail "rollback order: $(tr '\n' ' ' <"$tmp/ran")"
grep -q "Update 1.1.0-rc.1 was rolled back" "$tmp/calls" || fail "no rollback notification"
grep -q '\["report", "Report"\]' "$tmp/calls" || fail "no Report button"
grep -q "1.1.0-rc.1 keeps crashing" "$tmp/state/noobsbot-desktop/session.log" || fail "no session log"
grep -q "dbus-update-activation-environment --systemd WAYLAND_DISPLAY DISPLAY" "$tmp/calls" || fail "DISPLAY not exported"
grep -q pkexec "$tmp/calls" && fail "installed without install-at-login"
# Install at login: the compositor's marker makes the wrapper run the helper's passwordless mode, once, then 1.2.0.
: >"$tmp/state/noobsbot-desktop/install-at-login"
: >"$tmp/ran"
session
grep -q "pkexec --disable-internal-agent $tmp/lib/noobsbot-desktop-update --verified$" "$tmp/calls" || fail "helper not run: $(grep pkexec "$tmp/calls")"
[ "$(cat "$tmp/ran")" = 1.2.0 ] || fail "not started after the install: $(cat "$tmp/ran")"
grep -q "Updated to 1.2.0" "$tmp/calls" || fail "no Updated notification"
[ -e "$tmp/state/noobsbot-desktop/install-at-login" ] && fail "install-at-login kept"
ln -sfn "$tmp/lib/versions/1.0.0-beta.10/noobsbot-desktop" "$tmp/bin/noobsbot-desktop"
# A version shipping the watchdog (E6.4) runs under it, with the heartbeat pipe.
watchdog=$repo/target/debug/noobsbot-desktop-watchdog
if [ -x "$watchdog" ]; then
    ln -s "$watchdog" "$tmp/lib/versions/1.0.0-beta.10/noobsbot-desktop-watchdog"
    : >"$tmp/ran"
    session
    [ "$(cat "$tmp/ran")" = "1.0.0-beta.10 beats" ] || fail "not run under the watchdog"
else
    echo "skipped the watchdog check: cargo build first"
fi
echo "session: ok"

#!/bin/sh
# Checks the session wrapper's update rollback (E6.13) without touching the session: systemctl, gdbus and
# dbus-update-activation-environment are stubs, and the versions are fake compositors. Usage: sh scripts/test-session.sh
set -eu
repo=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/lib/versions/1.0.0" "$tmp/lib/versions/1.1.0"
for stub in systemctl gdbus dbus-update-activation-environment; do
    printf '#!/bin/sh\necho "%s $*" >>"%s/calls"\n' "$stub" "$tmp" >"$tmp/bin/$stub"
done
# 1.0.0 runs past the boot-ok marker; 1.1.0 quits at once. Each runs the `--` command, like the compositor.
printf '#!/bin/sh\necho 1.0.0${NOOBSBOT_HEARTBEAT_FD:+ beats} >>"%s/ran"\nshift; "$@"; sleep 1.5\n' "$tmp" >"$tmp/lib/versions/1.0.0/noobsbot-desktop"
printf '#!/bin/sh\necho 1.1.0 >>"%s/ran"\nexit 134\n' "$tmp" >"$tmp/lib/versions/1.1.0/noobsbot-desktop"
chmod +x "$tmp"/bin/* "$tmp"/lib/versions/*/noobsbot-desktop
export PATH="$tmp/bin:$PATH" NOOBSBOT_LIB="$tmp/lib" XDG_STATE_HOME="$tmp/state" NOOBSBOT_BOOT_OK_AFTER=1
session() { sh "$repo/data/system/usr/bin/noobsbot-desktop-session" 2>>"$tmp/log" || true; }
fail() { echo "FAIL: $*"; cat "$tmp/ran" "$tmp/log"; exit 1; }

ln -sfn "$tmp/lib/versions/1.0.0/noobsbot-desktop" "$tmp/bin/noobsbot-desktop"
session
[ "$(cat "$tmp/state/noobsbot-desktop/boot-ok")" = 1.0.0 ] || fail "1.0.0 not marked good"

# The update to 1.1.0: three crashes, then 1.0.0 again, and it stays there (with a notification).
ln -sfn "$tmp/lib/versions/1.1.0/noobsbot-desktop" "$tmp/bin/noobsbot-desktop"
: >"$tmp/ran"
session; session; session; session; session
[ "$(tr '\n' ' ' <"$tmp/ran")" = "1.1.0 1.1.0 1.1.0 1.0.0 1.0.0 " ] || fail "rollback order: $(tr '\n' ' ' <"$tmp/ran")"
grep -q "Update 1.1.0 was rolled back" "$tmp/calls" || fail "no rollback notification"
grep -q "1.1.0 keeps crashing" "$tmp/state/noobsbot-desktop/session.log" || fail "no session log"
grep -q "dbus-update-activation-environment --systemd WAYLAND_DISPLAY DISPLAY" "$tmp/calls" || fail "DISPLAY not exported"
# A version shipping the watchdog (E6.4) runs under it, with the heartbeat pipe.
watchdog=$repo/target/debug/noobsbot-desktop-watchdog
if [ -x "$watchdog" ]; then
    ln -s "$watchdog" "$tmp/lib/versions/1.0.0/noobsbot-desktop-watchdog"
    : >"$tmp/ran"
    session
    [ "$(cat "$tmp/ran")" = "1.0.0 beats" ] || fail "not run under the watchdog"
else
    echo "skipped the watchdog check: cargo build first"
fi
echo "session: ok"

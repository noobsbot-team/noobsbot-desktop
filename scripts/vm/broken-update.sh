#!/bin/sh
# Runs IN the VM: "installs" a 0.2.1 that dies at start, the way a broken update would, to test the session
# wrapper's rollback (E6.13).
set -eu
lib=/usr/lib/noobsbot-desktop
sudo mkdir -p "$lib/versions/0.2.1"
printf '#!/bin/sh\necho "broken 0.2.1 started" >&2\nexit 134\n' | sudo tee "$lib/versions/0.2.1/noobsbot-desktop" >/dev/null
sudo chmod 755 "$lib/versions/0.2.1/noobsbot-desktop"
sudo ln -sfn "$lib/versions/0.2.1/noobsbot-desktop" /usr/bin/noobsbot-desktop
cat ~/.local/state/noobsbot-desktop/boot-ok

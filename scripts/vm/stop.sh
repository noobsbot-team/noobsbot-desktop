#!/bin/sh
# Stops the test VM and the seed server started by boot.sh.
vm=$HOME/.cache/noobsbot-vm
for p in qemu http; do
    [ -f "$vm/$p.pid" ] && kill "$(cat "$vm/$p.pid")" 2>/dev/null
    rm -f "$vm/$p.pid"
done
echo stopped

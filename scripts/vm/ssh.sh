#!/bin/sh
# ssh into the test VM as tester (extra arguments are the remote command). scp: VMSCP=1 sh ssh.sh <src> <dst>
vm=$HOME/.cache/noobsbot-vm
opts="-i $vm/id_ed25519 -o StrictHostKeyChecking=no -o UserKnownHostsFile=$vm/known_hosts -o ConnectTimeout=5 -o LogLevel=ERROR"
if [ -n "${VMSCP:-}" ]; then
    # shellcheck disable=SC2086
    exec scp -P 2222 $opts "$@"
fi
# shellcheck disable=SC2086
exec ssh -p 2222 $opts tester@127.0.0.1 "$@"

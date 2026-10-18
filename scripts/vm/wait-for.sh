#!/bin/sh
# Waits (up to ${2:-1500} s) until the remote shell test $1 succeeds in the VM.
here=$(dirname "$0")
end=$(($(date +%s) + ${2:-1500}))
while [ "$(date +%s)" -lt "$end" ]; do
    sh "$here/ssh.sh" "$1" 2>/dev/null && { echo "ok: $1"; exit 0; }
    sleep 10
done
echo "timeout: $1"; exit 1

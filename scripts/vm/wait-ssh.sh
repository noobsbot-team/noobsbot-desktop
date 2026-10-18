#!/bin/sh
# Waits (up to ${1:-300} s) until the test VM answers ssh.
here=$(dirname "$0")
end=$(($(date +%s) + ${1:-300}))
while [ "$(date +%s)" -lt "$end" ]; do
    sh "$here/ssh.sh" true 2>/dev/null && { echo "ssh up"; exit 0; }
    sleep 3
done
echo "ssh timeout"; exit 1

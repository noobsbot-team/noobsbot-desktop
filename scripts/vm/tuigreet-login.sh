#!/bin/sh
# Types tester's password at tuigreet (username remembered) $1 times, waiting ${2:-6} s after each.
here=$(dirname "$0")
i=0
while [ "$i" -lt "${1:-1}" ]; do
    python3 "$here/qmp.py" type tester
    python3 "$here/qmp.py" key ret
    sleep "${2:-6}"
    i=$((i + 1))
done

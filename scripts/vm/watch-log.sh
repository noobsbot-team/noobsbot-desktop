#!/bin/sh
# Prints new "==>", warning/error and EXIT lines of a log file in the VM until EXIT appears. Usage: watch-log.sh <file>
here=$(dirname "$0")
seen=0
while true; do
    out=$(sh "$here/ssh.sh" "grep -naE '^.{0,12}==>|warning:|error:|EXIT ' $1" 2>/dev/null)
    n=$(printf '%s\n' "$out" | grep -c .)
    [ "$n" -gt "$seen" ] && printf '%s\n' "$out" | tail -n $((n - seen)) && seen=$n
    printf '%s\n' "$out" | grep -q 'EXIT ' && exit 0
    sleep 15
done

#!/bin/sh
# Takes $2 screenshots named $1-N.png, as fast as the VNC grab allows (to catch an animation such as the shake).
here=$(dirname "$0")
i=0
while [ "$i" -lt "${2:-6}" ]; do
    python3 "$here/vncshot.py" "$1-$i.png"
    i=$((i + 1))
done
# A strip of the crop $3 (WxH+X+Y) of each, top to bottom, as $1-strip.png.
[ -n "${3:-}" ] && magick "$1"-[0-9]*.png -crop "$3" -append "$1-strip.png"
exit 0

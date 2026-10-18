#!/bin/sh
# Boots the test VM headless. GPU=gl (default: virtio-vga-gl + egl-headless, virgl) or GPU=nogl (plain virtio-vga,
# the pixman fallback). ssh: ssh -p 2222 -i ~/.cache/noobsbot-vm/id_ed25519 tester@127.0.0.1
# Drive it with scripts/vm/qmp.py (screenshots, keys, mouse; under virgl screenshots come from the VNC socket). Writes qemu.pid; stop with scripts/vm/stop.sh.
set -eu
vm=$HOME/.cache/noobsbot-vm
cd "$vm"
# The NoCloud seed, served on the host loopback only (QEMU's user network reaches it as 10.0.2.2).
python3 -m http.server 8642 --bind 127.0.0.1 --directory "$vm/seed" >http.log 2>&1 &
echo $! >http.pid
if [ "${GPU:-gl}" = gl ]; then
    gpu="-device virtio-vga-gl,xres=1920,yres=1080 -display egl-headless,gl=on -vnc unix:$vm/vnc.sock"
else
    gpu="-device virtio-vga,xres=1920,yres=1080 -display none"
fi
# shellcheck disable=SC2086 # $gpu is several words
qemu-system-x86_64 -enable-kvm -cpu host -smp 4 -m 4G -machine q35 \
    -drive file=disk.qcow2,if=virtio,discard=unmap \
    $gpu \
    -device virtio-keyboard-pci -device virtio-tablet-pci \
    -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:2222-:22 \
    -smbios type=1,serial=ds=nocloud\;s=http://10.0.2.2:8642/ \
    -qmp unix:"$vm/qmp.sock",server,nowait \
    -serial file:"$vm/serial.log" \
    -daemonize -pidfile "$vm/qemu.pid"
echo "booted: pid $(cat qemu.pid)"

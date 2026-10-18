#!/bin/sh
# Makes the throwaway test VM (Arch cloud image overlay + cloud-init NoCloud seed) under ~/.cache/noobsbot-vm.
# Users: tester / tester (passwordless sudo, ssh key) and alice / alice (a second user for the greeter's row).
# Usage: sh scripts/vm/setup.sh   (then scripts/vm/boot.sh)
set -eu
vm=$HOME/.cache/noobsbot-vm
cd "$vm"
[ -f arch.qcow2 ] || curl -fL -o arch.qcow2 https://geo.mirror.pkgbuild.com/images/latest/Arch-Linux-x86_64-cloudimg.qcow2
[ -f id_ed25519 ] || ssh-keygen -q -t ed25519 -N "" -f id_ed25519 -C noobsbot-vm
rm -f disk.qcow2
qemu-img create -q -f qcow2 -F qcow2 -b arch.qcow2 disk.qcow2 20G
mkdir -p seed
echo "instance-id: noobsbot-$(date +%s)" >seed/meta-data
echo "local-hostname: noobsbot-vm" >>seed/meta-data
: >seed/vendor-data
cat >seed/user-data <<EOF
#cloud-config
ssh_pwauth: true
users:
  - name: tester
    gecos: Test User
    groups: [wheel]
    sudo: "ALL=(ALL) NOPASSWD:ALL"
    shell: /bin/bash
    lock_passwd: false
    plain_text_passwd: tester
    ssh_authorized_keys: ["$(cat id_ed25519.pub)"]
  - name: alice
    gecos: Alice Example
    shell: /bin/bash
    lock_passwd: false
    plain_text_passwd: alice
growpart: {mode: auto, devices: ["/"]}
resize_rootfs: true
EOF
echo "vm ready: $vm/disk.qcow2"

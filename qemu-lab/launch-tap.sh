#!/bin/bash
# Relaunch the lab VM with a TAP NIC so the host (and therefore the Android
# emulator at 10.0.2.2) can reach the guest's br-lan gate at 192.168.1.1:2121.
set -eu
LAB=/home/c08r4d0r/worktrees/tc-qemu-buy-verify/qemu-lab
BASE=/home/c08r4d0r/tollgate-virtual-lab-clean/images/openwrt-base.qcow2
OVL=$LAB/router-overlay.qcow2
[ -f "$OVL" ] || qemu-img create -f qcow2 -b "$BASE" -F qcow2 "$OVL" 2G >/dev/null
[ -f "$LAB/qemu.pid" ] && kill "$(cat "$LAB/qemu.pid")" 2>/dev/null || true
sleep 3
sudo -n ip link del tap-tg0 2>/dev/null || true
sudo -n ip tuntap add dev tap-tg0 mode tap user "$(id -un)"
sudo -n ip addr add 192.168.1.2/24 dev tap-tg0
sudo -n ip link set tap-tg0 up
rm -f "$LAB/serial.sock"
qemu-system-x86_64 \
  -machine q35,accel=kvm:tcg -cpu max -smp 2 -m 512 \
  -drive file="$OVL",if=virtio,format=qcow2 \
  -netdev tap,id=n0,ifname=tap-tg0,script=no,downscript=no \
  -device virtio-net-pci,netdev=n0,mac=52:54:00:12:34:56 \
  -netdev user,id=n1,net=10.0.2.0/24,hostfwd=tcp:127.0.0.1:3222-10.0.2.15:22 \
  -device virtio-net-pci,netdev=n1,mac=52:54:00:12:34:57 \
  -display none -serial unix:"$LAB/serial.sock",server=on,wait=off \
  -daemonize -pidfile "$LAB/qemu.pid"
echo "started pid=$(cat "$LAB/qemu.pid")"

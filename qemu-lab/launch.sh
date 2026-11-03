#!/bin/bash
# Isolated QEMU OpenWrt router VM, daemonized with a pidfile so it survives
# tool-session churn. Owns only its own overlay + serial socket.
set -eu
LAB=/home/c08r4d0r/worktrees/tc-qemu-buy-verify/qemu-lab
mkdir -p "$LAB"
BASE=/home/c08r4d0r/tollgate-virtual-lab-clean/images/openwrt-base.qcow2
OVL=$LAB/router-overlay.qcow2
[ -f "$OVL" ] || qemu-img create -f qcow2 -b "$BASE" -F qcow2 "$OVL" 2G >/dev/null
rm -f "$LAB/serial.sock"
qemu-system-x86_64 \
  -machine q35,accel=kvm:tcg -cpu max -smp 2 -m 512 \
  -drive file="$OVL",if=virtio,format=qcow2 \
  -netdev user,id=n0,net=10.0.3.0/24 \
  -device virtio-net-pci,netdev=n0,mac=52:54:00:12:34:56 \
  -netdev user,id=n1,net=10.0.2.0/24,hostfwd=tcp:127.0.0.1:3121-10.0.2.15:2121,hostfwd=tcp:127.0.0.1:3222-10.0.2.15:22 \
  -device virtio-net-pci,netdev=n1,mac=52:54:00:12:34:57 \
  -display none -serial unix:"$LAB/serial.sock",server=on,wait=off \
  -daemonize -pidfile "$LAB/qemu.pid"
echo "started pid=$(cat "$LAB/qemu.pid")"

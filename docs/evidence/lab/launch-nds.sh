#!/bin/bash
# Supported-stack lab: module's declared NDS (nodogsplash), no hand-written
# firewall shims. Customer client is a netns on the same L2 as the router.
set -eu
LAB=/home/c08r4d0r/worktrees/tc-qemu-buy-verify/qemu-lab
BASE=/home/c08r4d0r/tollgate-virtual-lab-clean/images/openwrt-base.qcow2
OVL=$LAB/router-nds.qcow2
[ -f "$OVL" ] || qemu-img create -f qcow2 -b "$BASE" -F qcow2 "$OVL" 4G >/dev/null

sudo -n ip link add br-tg type bridge 2>/dev/null || true
sudo -n ip link add veth-tg0 type veth peer name veth-tg1 2>/dev/null || true
sudo -n ip link set veth-tg1 netns tglab
sudo -n ip link set veth-tg0 master br-tg
sudo -n ip tuntap add dev tap-tg0 mode tap user "$(id -un)" 2>/dev/null || true
sudo -n ip link set tap-tg0 master br-tg
sudo -n ip addr flush dev br-tg
sudo -n ip addr add 10.99.99.200/24 dev br-tg
sudo -n ip link set br-tg up; sudo -n ip link set tap-tg0 up; sudo -n ip link set veth-tg0 up
sudo -n ip netns exec tglab ip link set lo up
sudo -n ip netns exec tglab ip addr add 10.99.99.50/24 dev veth-tg1
sudo -n ip netns exec tglab ip link set veth-tg1 up
sudo -n ip netns exec tglab ip route add default via 10.99.99.1
sudo -n ip netns exec tglab sh -c 'printf "nameserver 10.99.99.1\n" > /etc/resolv.conf'

rm -f "$LAB/serial.sock"
qemu-system-x86_64 \
  -machine q35,accel=kvm:tcg -cpu max -smp 2 -m 512 \
  -drive file="$OVL",if=virtio,format=qcow2 \
  -netdev tap,id=n0,ifname=tap-tg0,script=no,downscript=no \
  -device virtio-net-pci,netdev=n0,mac=52:54:00:12:34:56 \
  -netdev user,id=n1,net=10.0.2.0/24 \
  -device virtio-net-pci,netdev=n1,mac=52:54:00:12:34:57 \
  -display none -serial unix:"$LAB/serial.sock",server=on,wait=off \
  -daemonize -pidfile "$LAB/qemu.pid"
echo "pid=$(cat "$LAB/qemu.pid")  client=10.99.99.50  gw=10.99.99.1"

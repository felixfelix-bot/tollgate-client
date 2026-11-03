#!/bin/bash
# Add a real client to the lab: a network namespace on a host bridge with the
# VM's tap, so openNDS/fw4 can actually gate it (no NAT in the path).
set -eu
sudo -n ip netns add tglab 2>/dev/null || true
sudo -n ip link add br-tg type bridge 2>/dev/null || true
sudo -n ip link add veth-tg0 type veth peer name veth-tg1 2>/dev/null || true
sudo -n ip link set veth-tg1 netns tglab
sudo -n ip link set veth-tg0 master br-tg
sudo -n ip link set tap-tg0 master br-tg
sudo -n ip addr flush dev tap-tg0
sudo -n ip addr flush dev br-tg
sudo -n ip addr add 10.99.99.200/24 dev br-tg
sudo -n ip link set br-tg up
sudo -n ip link set veth-tg0 up
sudo -n ip link set tap-tg0 up
sudo -n ip netns exec tglab ip link set lo up
sudo -n ip netns exec tglab ip addr add 10.99.99.50/24 dev veth-tg1
sudo -n ip netns exec tglab ip link set veth-tg1 up
sudo -n ip netns exec tglab ip route add default via 10.99.99.1
sudo -n ip netns exec tglab ip -4 -br addr show veth-tg1
echo "client ready: 10.99.99.50 in netns tglab"

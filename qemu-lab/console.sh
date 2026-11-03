#!/bin/bash
# Send commands to the guest serial console and print what comes back.
# Usage: console.sh "cmd1" "cmd2" ...
SOCK=/home/c08r4d0r/worktrees/tc-qemu-buy-verify/qemu-lab/serial.sock
{
  sleep 1
  for c in "$@"; do printf '%s\n' "$c"; sleep 2; done
  printf 'echo __CMD_END__\n'
  sleep ${DRAIN:-10}
} | timeout 60 socat - UNIX-CONNECT:"$SOCK" 2>/dev/null | tr -d '\r'

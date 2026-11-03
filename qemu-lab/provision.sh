#!/bin/bash
# One-shot guest provisioning for the tmbg interop rig.
set -eu
LAB=/home/c08r4d0r/worktrees/tc-qemu-buy-verify/qemu-lab
ART=/home/c08r4d0r/worktrees/tc-qemu-buy-verify/artifacts
C="$LAB/console.sh"

# 1. host HTTP server for the module binary + fake ndsctl (guest pulls them)
pkill -f "http.server 18777" 2>/dev/null || true
sleep 1
cd "$ART"
cp /home/c08r4d0r/repos/tollgate-module-basic-go/tests/cloud-lab/fake-ndsctl.sh "$ART/ndsctl"
chmod +x "$ART/ndsctl"
nohup python3 -m http.server 18777 --bind 0.0.0.0 >"$LAB/httpd.log" 2>&1 &
sleep 2

bash "$C" "" \
  "mkdir -p /opt/tg/bin /etc/tollgate" \
  "wget -q -O /opt/tg/tollgate-module-basic-go http://10.0.2.2:18777/tollgate-module-basic-go && chmod +x /opt/tg/tollgate-module-basic-go && ls -l /opt/tg/tollgate-module-basic-go" \
  "wget -q -O /opt/tg/bin/ndsctl http://10.0.2.2:18777/ndsctl && chmod +x /opt/tg/bin/ndsctl && echo NDS_OK" \
  "cat > /etc/tollgate/config.json <<'EOF'
{\"config_version\":\"v0.0.10\",\"log_level\":\"debug\",\"accepted_mints\":[{\"url\":\"http://10.0.2.2:3338/\",\"price_per_step\":1,\"price_unit\":\"sat\",\"min_purchase_steps\":1}],\"profit_share\":[{\"factor\":1,\"identity\":\"owner\"}],\"metric\":\"bytes\",\"step_size\":22020096,\"reseller_mode\":false,\"private_encryption\":\"psk2\",\"admin_access\":\"private\",\"entry_ui\":\"board\"}
EOF
echo CONFIG_WRITTEN" \
  "wget -q -O - -T 4 http://10.0.2.2:3338/v1/info | head -c 100; echo" \
  "rm -f /tmp/ndsctl.log; touch /tmp/ndsctl.log" \
  "pgrep -f tollgate-module-basic-go >/dev/null && kill \$(pgrep -f tollgate-module-basic-go) || true; sleep 1; cd /opt/tg && NDSCTL_LOG=/tmp/ndsctl.log nohup /opt/tg/tollgate-module-basic-go > /tmp/tg.log 2>&1 & sleep 6; echo STARTED; tail -6 /tmp/tg.log"

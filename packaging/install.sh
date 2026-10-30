#!/bin/sh
# Install the Debian shell. No network access, no package manager, no daemon
# restart of anything that already exists: idempotent by construction.
set -eu

PREFIX=${PREFIX:-/usr/local}
HERE=$(cd "$(dirname "$0")/.." && pwd)
BIN="$HERE/target/release/tollgate-agent"

[ -x "$BIN" ] || { echo "build first: cargo build --release --bin tollgate-agent" >&2; exit 1; }
[ "$(id -u)" = 0 ] || { echo "install.sh must run as root" >&2; exit 1; }

# Idempotent: an existing service user is expected, not an error. (`set -e`
# turns a failed useradd into an aborted install, which is how this bit me.)
id -u tollgate >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin tollgate

install -Dm0755 "$BIN" "$PREFIX/bin/tollgate-agent"
install -Dm0644 "$HERE/packaging/systemd/tollgate-agent.service" /etc/systemd/system/tollgate-agent.service
install -Dm0644 "$HERE/packaging/systemd/tollgate-agent.timer"   /etc/systemd/system/tollgate-agent.timer
install -Dm0755 "$HERE/packaging/networkmanager/90-tollgate"     /etc/NetworkManager/dispatcher.d/90-tollgate
install -Dm0600 "$HERE/packaging/etc/tollgate/agent.env"         /etc/tollgate/agent.env
install -d -m0700 -o tollgate -g tollgate /var/lib/tollgate

systemctl daemon-reload
systemctl enable --now tollgate-agent.timer
systemctl start tollgate-agent.service || true   # a first run now, not in 5 min

echo "installed. journal: journalctl -u tollgate-agent -n 50"

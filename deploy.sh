#!/bin/sh
# Deploy ac-client to an OpenWrt device (aarch64).
# Usage: ./deploy.sh <device-ip> [ssh-user]
#
# The binary is statically linked (musl) — no runtime deps on the device.
# On first deploy it installs the init.d script and enables autostart.

set -e

DEVICE="${1:?Usage: $0 <device-ip> [user]}"
USER="${2:-root}"
BINARY="target/aarch64-unknown-linux-musl/release/ac-client"
REMOTE_BIN="/usr/bin/ac-client"
INIT_SCRIPT="/etc/init.d/ac-client"

if [ ! -f "$BINARY" ]; then
    echo "Binary not found: $BINARY"
    echo "Run: ~/.cargo/bin/cargo build --release --target aarch64-unknown-linux-musl"
    exit 1
fi

echo "==> Deploying to $USER@$DEVICE ..."

# Copy binary (no sftp-server on OpenWrt — use dd pipe instead of scp)
dd if="$BINARY" | ssh "$USER@$DEVICE" "dd of=/tmp/ac-client-new && mv /tmp/ac-client-new $REMOTE_BIN && chmod +x $REMOTE_BIN"

# Install init.d script if not present
ssh "$USER@$DEVICE" "test -f $INIT_SCRIPT" 2>/dev/null || {
    echo "==> Installing init.d script ..."
    ssh "$USER@$DEVICE" "cat > $INIT_SCRIPT" <<'INITEOF'
#!/bin/sh /etc/rc.common
START=95
STOP=5
USE_PROCD=1
PROG=/usr/bin/ac-client

start_service() {
    procd_open_instance
    procd_set_param command $PROG --uci
    procd_set_param respawn 30 5 0
    procd_set_param stderr 1
    procd_close_instance
}
INITEOF
    ssh "$USER@$DEVICE" "chmod +x $INIT_SCRIPT && $INIT_SCRIPT enable"
}

# Ensure config dir and default config exist
ssh "$USER@$DEVICE" "mkdir -p /etc/apclient"
ssh "$USER@$DEVICE" "test -f /etc/apclient/ac_client.conf" 2>/dev/null || {
    echo "==> Writing default config (edit /etc/apclient/ac_client.conf on device) ..."
    ssh "$USER@$DEVICE" "cat > /etc/apclient/ac_client.conf" <<'CONFEOF'
# ac-client configuration
# server_host = usp.optimcloud.com
# server_port = 443
# mac_addr    =          # leave empty for auto-detection
# log_syslog  = true
CONFEOF
}

# Restart service (stop first so the binary is not busy when replaced)
echo "==> Restarting ac-client ..."
ssh "$USER@$DEVICE" "kill \$(pidof ac-client) 2>/dev/null; true"
ssh "$USER@$DEVICE" "$INIT_SCRIPT restart 2>/dev/null || $REMOTE_BIN --uci &"

echo "==> Done. Logs: ssh $USER@$DEVICE logread -f | grep ac-client"

#!/usr/bin/env bash
# ==============================================================================
# Quick deploy script: Downloads and runs latest cfsolve-server on any Linux VPS
# Usage:
#   curl -sSL https://raw.githubusercontent.com/shinas101/cfsolve/master/scripts/deploy-vps.sh | bash
# ==============================================================================
set -euo pipefail

REPO="shinas101/cfsolve"
INSTALL_DIR="/opt/cfsolve"
PORT="${PORT:-8080}"
HOST="${HOST:-0.0.0.0}"

echo ">>> Detecting architecture..."
ARCH="$(uname -m)"
case "$ARCH" in
    x86_64) ASSET_ARCH="x86_64" ;;
    aarch64|arm64) ASSET_ARCH="aarch64" ;;
    *) echo "Error: Unsupported architecture $ARCH" >&2; exit 1 ;;
esac

DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/cfsolve-linux-${ASSET_ARCH}.tar.gz"

echo ">>> Creating install directory: ${INSTALL_DIR}"
sudo mkdir -p "${INSTALL_DIR}"
sudo chown -R "$(id -u):$(id -g)" "${INSTALL_DIR}"
cd "${INSTALL_DIR}"

echo ">>> Downloading latest release from ${DOWNLOAD_URL}..."
curl -f -sSL "${DOWNLOAD_URL}" -o cfsolve.tar.gz
tar -xzf cfsolve.tar.gz
rm -f cfsolve.tar.gz

chmod +x cfsolve-server 2>/dev/null || true

echo ">>> Installing binary to /usr/local/bin..."
sudo cp cfsolve-server /usr/local/bin/

echo ">>> Creating systemd service /etc/systemd/system/cfsolve.service..."
sudo tee /etc/systemd/system/cfsolve.service > /dev/null <<EOF
[Unit]
Description=cfsolve HTTP API Server
After=network.target

[Service]
Type=simple
User=$(whoami)
WorkingDirectory=${INSTALL_DIR}
ExecStart=/usr/local/bin/cfsolve-server --host ${HOST} --port ${PORT}
Restart=always
RestartSec=5
Environment=HOST=${HOST}
Environment=PORT=${PORT}

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable cfsolve
sudo systemctl restart cfsolve

echo ""
echo "============================================================"
echo " cfsolve-server successfully installed and running!"
echo " Status:    sudo systemctl status cfsolve"
echo " Logs:      sudo journalctl -u cfsolve -f"
echo " Endpoint:  http://${HOST}:${PORT}/health"
echo "============================================================"

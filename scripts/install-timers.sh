#!/usr/bin/env bash
#
# install-timers.sh — Install systemd user units for KB maintenance.
#
# Installs:
#   - kb-server.service  — HTTP daemon (always-on MCP server)
#   - kb-drain.timer     — Queue drain every 5 minutes
#   - kb-backup.timer    — Weekly JSONL backup
#
# Usage:
#   bash scripts/install-timers.sh
#
# To uninstall:
#   systemctl --user disable --now kb-server kb-drain.timer kb-backup.timer
#   rm ~/.config/systemd/user/kb-*.{service,timer}
#   systemctl --user daemon-reload
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UNIT_DIR="$HOME/.config/systemd/user"

mkdir -p "$UNIT_DIR"

# Copy all unit files
for unit in kb-server.service kb-drain.service kb-drain.timer kb-backup.service kb-backup.timer; do
    cp "$SCRIPT_DIR/$unit" "$UNIT_DIR/$unit"
    echo "  Installed $unit"
done

systemctl --user daemon-reload

# Enable and start
systemctl --user enable --now kb-server.service
systemctl --user enable --now kb-drain.timer
systemctl --user enable --now kb-backup.timer

echo ""
echo "All units installed and started."
echo ""
echo "Check status:"
echo "  systemctl --user status kb-server"
echo "  systemctl --user list-timers"
echo ""
echo "View logs:"
echo "  journalctl --user -u kb-server -f"
echo ""
echo "Crontab alternative (if not using systemd):"
echo "  */5 * * * * $SCRIPT_DIR/drain.sh >> /tmp/kb-drain.log 2>&1"
echo "  0 3 * * 0  $SCRIPT_DIR/backup.sh >> /tmp/kb-backup.log 2>&1"

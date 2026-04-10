#!/usr/bin/env bash
#
# install.sh — Build and install smriti, wire into Claude Code settings.
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

echo "Building smriti..."
cargo install --path "$ROOT_DIR"

# Verify binary
if ! command -v smriti &>/dev/null; then
    echo "Error: smriti not found in PATH after install" >&2
    exit 1
fi

echo "smriti installed at $(which smriti)"

# Create data directories
mkdir -p "$ROOT_DIR/data" "$ROOT_DIR/queue" "$ROOT_DIR/archive"/{meetings,slack,discord}

# Wire into Claude Code settings
SETTINGS_FILE="$HOME/.claude/settings.json"
mkdir -p "$(dirname "$SETTINGS_FILE")"

if [[ -f "$SETTINGS_FILE" ]]; then
    # Check if kb server is already configured
    if command -v jq &>/dev/null && jq -e '.mcpServers.kb' "$SETTINGS_FILE" &>/dev/null; then
        echo "MCP server 'kb' already configured in $SETTINGS_FILE"
        exit 0
    fi
fi

KB_SERVER_PATH="$(which smriti)"

# Generate the MCP config snippet
cat <<EOF

Add the following to $SETTINGS_FILE under "mcpServers":

  "kb": {
    "command": "$KB_SERVER_PATH",
    "args": [],
    "env": {
      "KB_DATA_DIR": "$ROOT_DIR",
      "KB_CONFIG_DIR": "$ROOT_DIR",
      "KB_QUEUE_DIR": "$ROOT_DIR/queue"
    }
  }

EOF

echo "Done."

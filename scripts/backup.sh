#!/usr/bin/env bash
#
# backup.sh — Export the knowledge base to a JSONL backup file.
#
# Calls the kb-server binary directly (no LLM needed) to produce
# a portable JSONL export. Rotates old backups to keep disk usage bounded.
#
# Usage:
#   bash scripts/backup.sh
#
# Environment:
#   KB_DATA_DIR    — database directory (default: ./data)
#   KB_CONFIG_DIR  — config directory (default: project root)
#   KB_MAX_BACKUPS — number of backups to keep (default: 10)
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

export KB_DATA_DIR="${KB_DATA_DIR:-$ROOT_DIR/data}"
export KB_CONFIG_DIR="${KB_CONFIG_DIR:-$ROOT_DIR}"
BACKUP_DIR="${KB_BACKUP_DIR:-$ROOT_DIR/archive/exports}"
MAX_BACKUPS="${KB_MAX_BACKUPS:-10}"

mkdir -p "$BACKUP_DIR"

echo "Starting knowledge base backup..."
echo "  Data dir: $KB_DATA_DIR"
echo "  Backup dir: $BACKUP_DIR"

# Use the MCP server binary if available; otherwise fall back to claude CLI.
if command -v kb-server &>/dev/null; then
    kb-server --backup
elif command -v claude &>/dev/null; then
    claude --mcp-server "${KB_MCP_SERVER:-kb}" \
        -p "Call kb_export to create a backup. Use default settings. Report the file path."
else
    echo "Error: neither kb-server nor claude found in PATH" >&2
    exit 1
fi

# Rotate old backups (keep last N).
if [[ -d "$BACKUP_DIR" ]]; then
    backup_count=$(find "$BACKUP_DIR" -name "kb_export_*.jsonl" -type f 2>/dev/null | wc -l)
    if (( backup_count > MAX_BACKUPS )); then
        find "$BACKUP_DIR" -name "kb_export_*.jsonl" -type f -printf '%T@ %p\n' \
            | sort -n \
            | head -n "$(( backup_count - MAX_BACKUPS ))" \
            | cut -d' ' -f2- \
            | xargs rm -f
        echo "Rotated old backups. Kept last $MAX_BACKUPS."
    fi
fi

echo "Backup complete."

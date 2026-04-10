#!/usr/bin/env bash
#
# drain.sh — Process the pending memory queue via an LLM session.
#
# This script reads the queue and passes it to an LLM with the memory session
# prompt. The LLM calls kb_* MCP tools to process each instruction.
#
# Usage:
#   bash scripts/drain.sh                  # uses claude (default)
#   LLM_CMD="ollama run llama3" bash scripts/drain.sh  # use any LLM
#
# Environment:
#   KB_QUEUE_DIR  — queue directory (default: ./queue)
#   LLM_CMD       — command to run the LLM (default: claude)
#   KB_MCP_SERVER — name of the MCP server config (default: kb)
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

KB_QUEUE_DIR="${KB_QUEUE_DIR:-$ROOT_DIR/queue}"
LLM_CMD="${LLM_CMD:-claude}"
KB_MCP_SERVER="${KB_MCP_SERVER:-kb}"
QUEUE_FILE="$KB_QUEUE_DIR/pending.jsonl"
PROMPT_FILE="$SCRIPT_DIR/memory-session-prompt.txt"
LOCKFILE="/tmp/kb-drain.lock"

# Prevent concurrent drain runs. If another drain is already running (e.g.
# cron fired while a previous run is still in progress), skip this invocation.
exec 9>"$LOCKFILE"
if ! flock -n 9; then
    echo "Drain already running (lock held by another process), skipping."
    exit 0
fi

# Check queue exists and has entries
if [[ ! -f "$QUEUE_FILE" ]] || [[ ! -s "$QUEUE_FILE" ]]; then
    echo "Queue empty, nothing to process."
    exit 0
fi

ENTRY_COUNT=$(wc -l < "$QUEUE_FILE" | tr -d ' ')
echo "Processing $ENTRY_COUNT queued entries..."

# Read the queue content
QUEUE_CONTENT=$(cat "$QUEUE_FILE")

# Read the system prompt
if [[ ! -f "$PROMPT_FILE" ]]; then
    echo "Error: memory session prompt not found at $PROMPT_FILE" >&2
    exit 1
fi

SYSTEM_PROMPT=$(cat "$PROMPT_FILE")

# Run the LLM with MCP server access
$LLM_CMD \
    --mcp-server "$KB_MCP_SERVER" \
    -p "$SYSTEM_PROMPT" \
    "Process the following queued memory instructions. Each line is a JSON object with 'instruction', 'source_project', and 'queued_at' fields.

$QUEUE_CONTENT"

# Clear processed entries
# Move to a processed archive instead of deleting
PROCESSED_DIR="$KB_QUEUE_DIR/processed"
mkdir -p "$PROCESSED_DIR"
mv "$QUEUE_FILE" "$PROCESSED_DIR/$(date +%Y%m%d_%H%M%S).jsonl"

echo "Done. Processed $ENTRY_COUNT entries."

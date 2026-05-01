#!/usr/bin/env bash
# smriti system-level install (multi-user shared DB via HTTP daemon)
#
# Run as: sudo bash scripts/install-system.sh
# Idempotent. Assumes group `smriti` exists with stark + leanscale as members.

set -euo pipefail

if [[ $EUID -ne 0 ]]; then
  echo "ERROR: must run as root (sudo bash $0)"
  exit 1
fi

REPO_DIR="${REPO_DIR:-/home/stark/projects/system/smriti}"
BIN_SRC="$REPO_DIR/target/release/smriti"
BIN_DST="/usr/local/bin/smriti"
DATA_DIR="/var/lib/smriti"
ETC_DIR="/etc/smriti"

# 1. system user
if ! id smriti &>/dev/null; then
  echo "[+] creating system user 'smriti'"
  useradd -r -s /usr/sbin/nologin -d "$DATA_DIR" -g smriti smriti
else
  echo "[=] user 'smriti' already exists"
fi

# 2. dir layout (idempotent)
echo "[+] ensuring directory layout under $DATA_DIR"
install -d -o smriti -g smriti -m 2775 "$DATA_DIR" \
  "$DATA_DIR/data" \
  "$DATA_DIR/queue" \
  "$DATA_DIR/archive" \
  "$DATA_DIR/archive/exports"

install -d -o root -g smriti -m 0755 "$ETC_DIR"

# default ACL: new files inherit group rwx
if command -v setfacl &>/dev/null; then
  setfacl -d -m g:smriti:rwx "$DATA_DIR"
fi

# chown anything pre-existing to smriti:smriti (safe — group already correct)
chown -R smriti:smriti "$DATA_DIR"
chmod 2775 "$DATA_DIR" "$DATA_DIR/data" "$DATA_DIR/queue" "$DATA_DIR/archive" "$DATA_DIR/archive/exports"

# 3. binary
if [[ ! -x "$BIN_SRC" ]]; then
  echo "ERROR: $BIN_SRC missing — run 'cargo build --release' first"
  exit 1
fi
echo "[+] installing binary $BIN_SRC -> $BIN_DST"
install -m 0755 "$BIN_SRC" "$BIN_DST"

# 4. config — copy if absent, leave alone if user-edited
if [[ ! -f "$ETC_DIR/config.toml" ]]; then
  echo "[+] seeding $ETC_DIR/config.toml from repo"
  install -o root -g smriti -m 0644 "$REPO_DIR/config.toml" "$ETC_DIR/config.toml"
else
  echo "[=] $ETC_DIR/config.toml already exists — not overwriting"
fi

# 5. secrets template (don't overwrite if present)
if [[ ! -f "$ETC_DIR/secrets.env" ]]; then
  echo "[+] writing $ETC_DIR/secrets.env template"
  cat > "$ETC_DIR/secrets.env" <<'EOF'
# smriti runtime secrets — NOT checked in. Mode 0640, root:smriti.
# Loaded by systemd unit via EnvironmentFile=
# VOYAGE_API_KEY=
EOF
  chown root:smriti "$ETC_DIR/secrets.env"
  chmod 0640 "$ETC_DIR/secrets.env"
else
  echo "[=] $ETC_DIR/secrets.env already exists — not overwriting"
fi

# 5b. bearer auth tokens (ADR-027) — generate if absent
TOKENS_FILE="$ETC_DIR/tokens.toml"
if [[ ! -f "$TOKENS_FILE" ]]; then
  echo "[+] generating bearer auth tokens in $TOKENS_FILE"
  if ! command -v openssl &>/dev/null; then
    echo "ERROR: openssl not found — cannot generate tokens; install openssl and re-run"
    exit 1
  fi
  TOK_STARK=$(openssl rand -hex 32)
  TOK_LEANSCALE=$(openssl rand -hex 32)
  cat > "$TOKENS_FILE" <<EOF
# smriti bearer auth tokens (ADR-027) — NOT checked in. Mode 0640, root:smriti.
# Each value is a pre-shared secret; rotate by replacing the value and
# restarting smriti (systemctl restart smriti).
#
# Add to MCP client (per user, run once):
#   claude mcp add smriti --transport http http://127.0.0.1:47821/mcp \\
#     --header "Authorization: Bearer <your-token>" -s user

[tokens]
stark     = "$TOK_STARK"
leanscale = "$TOK_LEANSCALE"
EOF
  chown root:smriti "$TOKENS_FILE"
  chmod 0640 "$TOKENS_FILE"
  echo "[!] IMPORTANT: distribute tokens to users — do NOT share tokens between users"
  echo "    stark     token: $TOK_STARK"
  echo "    leanscale token: $TOK_LEANSCALE"
else
  echo "[=] $TOKENS_FILE already exists — not overwriting (run: cat $TOKENS_FILE)"
fi

# 6. systemd unit
UNIT="/etc/systemd/system/smriti.service"
echo "[+] writing $UNIT"
cat > "$UNIT" <<'EOF'
[Unit]
Description=Smriti MCP memory server (HTTP daemon, multi-user)
Documentation=https://github.com/dhruvpatel-10/smriti
After=network.target

[Service]
Type=simple
User=smriti
Group=smriti

Environment=KB_DATA_DIR=/var/lib/smriti
Environment=KB_CONFIG_DIR=/etc/smriti
Environment=KB_TRANSPORT=http
Environment=KB_HTTP_PORT=47821
Environment=RUST_LOG=info
EnvironmentFile=-/etc/smriti/secrets.env

ExecStart=/usr/local/bin/smriti

Restart=on-failure
RestartSec=5
TimeoutStopSec=20

# hardening
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/var/lib/smriti
ProtectHome=true
PrivateTmp=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictNamespaces=true
RestrictRealtime=true
LockPersonality=true
MemoryDenyWriteExecute=false   # rmcp/tokio JIT? keep false to be safe initially
SystemCallArchitectures=native

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload

echo
echo "==== install complete ===="
echo "  binary:   $BIN_DST"
echo "  data:     $DATA_DIR"
echo "  config:   $ETC_DIR/config.toml"
echo "  secrets:  $ETC_DIR/secrets.env (edit to add VOYAGE_API_KEY)"
echo "  unit:     $UNIT"
echo
echo "Next:"
echo "  1. (optional) edit $ETC_DIR/secrets.env"
echo "  2. systemctl enable --now smriti"
echo "  3. systemctl status smriti"
echo "  4. curl -s http://127.0.0.1:47821/mcp  # smoke check"
echo "  5. members of group smriti must re-login or 'newgrp smriti' once"

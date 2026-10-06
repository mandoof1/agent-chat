#!/usr/bin/env bash
# Optional: start Agent Chat automatically when you log in, so routines can run on time.
# This starts ONLY this app (about 60 MB of RAM). It never starts llama-server.
#   ./install-service.sh           install and start
#   ./install-service.sh --remove  stop and uninstall
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
UNIT="$HOME/.config/systemd/user/agent-chat.service"

if [[ "${1:-}" == "--remove" ]]; then
  systemctl --user disable --now agent-chat.service 2>/dev/null || true
  rm -f "$UNIT"
  systemctl --user daemon-reload
  echo "Removed. Start it by hand with $DIR/run.sh"
  exit 0
fi

if ss -ltn 2>/dev/null | grep -q ':8765\b'; then
  echo "Something is already using port 8765 (probably run.sh). Stop it first, then run this again."
  exit 1
fi

mkdir -p "$(dirname "$UNIT")"
cat > "$UNIT" <<UNITEOF
[Unit]
Description=Agent Chat (local multi-agent chat UI)
After=network.target

[Service]
WorkingDirectory=$DIR
ExecStart=$(command -v uv) run uvicorn app.main:app --host 127.0.0.1 --port 8765 --log-level warning
Restart=on-failure

[Install]
WantedBy=default.target
UNITEOF
systemctl --user daemon-reload
systemctl --user enable --now agent-chat.service
echo "Installed and running at http://127.0.0.1:8765"
echo "Status: systemctl --user status agent-chat   ·   Logs: journalctl --user -u agent-chat"
echo "Remove: $0 --remove"

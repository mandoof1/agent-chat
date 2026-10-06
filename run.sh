#!/usr/bin/env bash
# Start Agent Chat on http://127.0.0.1:8765
# This does NOT start llama-server. Load your model first, then open the UI.
# On a RAM-tight box, opening a browser during model load can trigger the OOM killer.
set -euo pipefail
cd "$(dirname "$0")"
PORT="${PORT:-8765}"

BASE_URL=$(python3 -c "import json;print(json.load(open('data/settings.json'))['base_url'])" 2>/dev/null || echo "http://127.0.0.1:8080/v1")
if curl -sf -m 2 "$BASE_URL/models" >/dev/null; then
  echo "Model server: online at $BASE_URL"
else
  echo "Model server: not reachable at $BASE_URL (start llama-server; the UI will pick it up)"
fi
SEARCH_URL=$(python3 -c "import json;print(json.load(open('data/settings.json')).get('search_url',''))" 2>/dev/null || true)
if [[ -z "$SEARCH_URL" ]]; then
  echo "Web search:   DuckDuckGo scraping (often blocked; set Settings -> Search engine URL to SearXNG)"
elif curl -sf -m 2 "$SEARCH_URL/healthz" >/dev/null; then
  echo "Web search:   SearXNG online at $SEARCH_URL"
else
  echo "Web search:   SearXNG not reachable at $SEARCH_URL (systemctl --user start searxng)"
fi
BROWSER_EXE=$(python3 -c "import json;print(json.load(open('data/settings.json')).get('browser_executable','/usr/bin/brave'))" 2>/dev/null || echo "/usr/bin/brave")
if [[ -x "$BROWSER_EXE" ]]; then
  echo "Browser:      Brave at $BROWSER_EXE (Browser agent ready)"
else
  echo "Browser:      $BROWSER_EXE not found (set Settings -> Browser executable for the Browser agent)"
fi
echo "Agent Chat:   http://127.0.0.1:$PORT"
exec uv run uvicorn app.main:app --host 127.0.0.1 --port "$PORT" --log-level warning

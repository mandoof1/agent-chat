#!/usr/bin/env bash
# Run every test that needs no GPU: starts the mock model servers and the app on scratch
# ports/folders, runs the unit tests, the API end-to-end suites and the browser UI suite, then
# stops everything. Usage: tests/run_all.sh [--ui | --no-ui]   (the UI suite needs Brave + Playwright)
set -uo pipefail
cd "$(dirname "$0")/.."
WITH_UI=auto
[[ "${1:-}" == "--ui" ]] && WITH_UI=yes
[[ "${1:-}" == "--no-ui" ]] && WITH_UI=no
BRAVE="${BROWSER_EXECUTABLE:-/usr/bin/brave}"
if [[ $WITH_UI == auto ]]; then [[ -x "$BRAVE" ]] && WITH_UI=yes || WITH_UI=no; fi

TMP=$(mktemp -d /tmp/agent-chat-tests.XXXXXX)
export AGENT_CHAT_DATA="$TMP/data" AGENT_CHAT_WORKSPACE="$TMP/workspace" AGENT_CHAT_ROUTINE_TICK=1
export AC_URL="http://127.0.0.1:8767"
PIDS=()
start() { "$@" > "$TMP/$(echo "$*" | tr -c 'A-Za-z0-9' '_' | cut -c1-60).log" 2>&1 & PIDS+=($!); }
cleanup() { for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; wait 2>/dev/null; echo "logs and data: $TMP"; }
trap cleanup EXIT

start uv run uvicorn tests.mock_llm:app --port 8766
MOCK_NCTX=3000 start uv run uvicorn tests.mock_llm:app --port 8768
MOCK_OVERFLOW_CHARS=6000 start uv run uvicorn tests.mock_llm:app --port 8769
MOCK_NO_VISION=1 start uv run uvicorn tests.mock_llm:app --port 8770
start uv run uvicorn app.main:app --port 8767 --log-level warning
for i in $(seq 1 40); do curl -sf "$AC_URL/api/health" >/dev/null && curl -sf http://127.0.0.1:8770/v1/models >/dev/null && break; sleep 0.5; done
curl -sf -X PUT "$AC_URL/api/settings" -H 'content-type: application/json' -H 'X-Agent-Chat: 1' \
     -d '{"base_url":"http://127.0.0.1:8766/v1"}' >/dev/null || { echo "the app did not start; see $TMP"; exit 1; }

FAILED=()
run() { local name=$1; shift; echo "=== $name"; if "$@" > "$TMP/$name.out" 2>&1; then echo "    ok"; else echo "    FAILED (see $TMP/$name.out)"; tail -15 "$TMP/$name.out"; FAILED+=("$name"); fi; }
run unit      uv run python tests/test_unit.py
run e2e       uv run python tests/e2e.py
run features  uv run python tests/e2e_features.py
run cutoff    uv run python tests/e2e_cutoff.py
run ctx       uv run python tests/e2e_ctx.py
run queue     env PYTHONPATH=. uv run python tests/e2e_queue.py
run memory    uv run python tests/e2e_memory.py
run routines  uv run python tests/e2e_routines.py
run v2        uv run python tests/e2e_v2.py
run calendar  env TEST_ICS="$TMP/test.ics" uv run python tests/e2e_calendar.py
if [[ $WITH_UI == yes ]]; then
  run browser env PYTHONPATH=. uv run python tests/e2e_browser.py
  run ui      uv run python tests/e2e_ui.py
else
  echo "=== browser + ui: skipped (no Brave at $BRAVE; set BROWSER_EXECUTABLE or pass --ui)"
fi
if command -v cargo >/dev/null 2>&1 || [[ -x "$HOME/.cargo/bin/cargo" ]]; then
  export PATH="$HOME/.cargo/bin:$PATH"
  echo "=== tui: building"
  if (cd tui && cargo build --release -q && cargo test -q 2>/dev/null | grep -q "test result: ok"); then
    run tui env XDG_CONFIG_HOME="$TMP/xdg" uv run --with pyte python tests/e2e_tui.py
  else
    echo "    FAILED (cargo build/test; see above)"; FAILED+=(tui-build)
  fi
else
  echo "=== tui: skipped (no cargo; install Rust to test the terminal client)"
fi

echo
if ((${#FAILED[@]})); then echo "FAILED: ${FAILED[*]}"; exit 1; else echo "all suites passed"; fi

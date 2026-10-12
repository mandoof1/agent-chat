#!/usr/bin/env bash
# The installer, end to end, on this machine: install the committed code of this checkout with
# site/install.sh into a scratch folder, start the app with the launcher, check it answers and
# serves the UI, stop it, uninstall. CI runs it on Linux and macOS.
#   tests/install_smoke.sh            (AGENT_CHAT_NO_TUI=0 also builds the terminal client)
set -euo pipefail
cd "$(dirname "$0")/.."
T=$(mktemp -d "${TMPDIR:-/tmp}/agent-chat-install.XXXXXX")
PORT=${AGENT_CHAT_PORT:-8795}
version=$(sed -n 's/^VERSION = "\(.*\)"/\1/p' app/main.py)
git archive --format=tar.gz --prefix=agent-chat/ -o "$T/src.tar.gz" HEAD

export AGENT_CHAT_HOME="$T/home" AGENT_CHAT_BIN="$T/bin" AGENT_CHAT_PORT="$PORT"
export XDG_STATE_HOME="$T/state" XDG_CONFIG_HOME="$T/config" XDG_CACHE_HOME="$T/cache"  # the launcher's log, pid file and build cache: not yours
export AGENT_CHAT_VERSION="$version" AGENT_CHAT_SRC_URL="file://$T/src.tar.gz"
export AGENT_CHAT_NO_TUI="${AGENT_CHAT_NO_TUI:-1}"
[ "$AGENT_CHAT_NO_TUI" = 1 ] || export AGENT_CHAT_BUILD_TUI=1  # this commit's client, not a release's
fail() { echo "FAILED: $*"; "$T/bin/agent-chat" logs 2>/dev/null | tail -30 || true; exit 1; }

echo "== install $version from this checkout into $T"
bash site/install.sh || fail "the installer"
status=$("$T/bin/agent-chat" status) || true  # exit 3: installed, not running
case "$status" in *"Agent Chat $version"*) ;; *) fail "status doesn't name $version: $status" ;; esac
if [ "$AGENT_CHAT_NO_TUI" != 1 ]; then
  case "$("$T/bin/agent-chat-tui" --version)" in *"$version"*) ;; *) fail "the terminal client isn't $version" ;; esac
fi

echo "== start, answer, stop"
"$T/bin/agent-chat" start || fail "start"
curl -fsS "http://127.0.0.1:$PORT/api/health" >/dev/null || fail "no answer on /api/health"
page=$(curl -fsS "http://127.0.0.1:$PORT/") || fail "no web UI"
case "$page" in *"<title>"*) ;; *) fail "the web UI isn't served" ;; esac
"$T/bin/agent-chat" stop || fail "stop"
if curl -fsS "http://127.0.0.1:$PORT/api/health" >/dev/null 2>&1; then fail "still answering after stop"; fi

echo "== uninstall"
"$T/bin/agent-chat" uninstall --yes --purge || fail "uninstall"
[ ! -e "$T/bin/agent-chat" ] && [ ! -e "$T/home/app" ] || fail "uninstall left files behind"
rm -rf "$T"
echo "install smoke test passed"

#!/usr/bin/env bash
# Agent Chat installer: installs or updates Agent Chat for the current user (no sudo).
#
#   curl -fsSL https://agent-chat-i4sv.onrender.com/install.sh | bash
#
# What goes where:
#   ~/.local/share/agent-chat/app        the app itself (replaced as a whole on every update)
#   ~/.local/share/agent-chat/data       your chats, agents, memory and settings (never touched)
#   ~/.local/share/agent-chat/workspace  the agents' default working folder (never touched)
#   ~/.local/bin/agent-chat              the launcher: `agent-chat`, `agent-chat web`, ...
#   ~/.local/bin/agent-chat-tui          the terminal client
# Run it again to update. Remove with `agent-chat uninstall` (keeps data unless --purge).
#
# Optional environment:
#   AGENT_CHAT_VERSION=0.3.0   install this release instead of the latest
#   AGENT_CHAT_HOME=DIR        install root, a folder of its own (default $XDG_DATA_HOME/agent-chat,
#                              else ~/.local/share/agent-chat)
#   AGENT_CHAT_BIN=DIR         where the launcher and the terminal client go (default ~/.local/bin)
#   AGENT_CHAT_NO_TUI=1        skip the terminal client (the web UI works without it)
#   AGENT_CHAT_BUILD_TUI=1     build the terminal client with cargo even where a prebuilt one exists
#   AGENT_CHAT_NO_UV=1         never install uv; stop with an error if it is missing
#   AGENT_CHAT_ALLOW_ROOT=1    install even though this runs as root
#   Download locations, for mirrors and testing: AGENT_CHAT_SITE, AGENT_CHAT_REPO,
#   AGENT_CHAT_LATEST_URL, AGENT_CHAT_SRC_URL, AGENT_CHAT_TUI_URL, AGENT_CHAT_TUI_SHA256_URL,
#   AGENT_CHAT_UV_INSTALL_URL. https sources are never followed to plain http.
#
# Everything runs inside main(), so a download cut off halfway does nothing.

# Written so that any sh can parse it: `curl ... | sh` with sh = dash stops here, cleanly.
if [ -z "${BASH_VERSION:-}" ]; then
  echo "error: this installer needs bash. Run it with: curl -fsSL <address>/install.sh | bash" >&2
  exit 1
fi

set -euo pipefail

# The site that serves this script. Change it here (one place) once the site has its final address.
DEFAULT_SITE="https://agent-chat-i4sv.onrender.com"
# Installed when the latest release can't be looked up (offline GitHub, rate limits).
PINNED_VERSION="v0.3.0"

SITE="${AGENT_CHAT_SITE:-$DEFAULT_SITE}"
SITE="${SITE%/}"
REPO="${AGENT_CHAT_REPO:-https://github.com/mandoof1/agent-chat}"
REPO="${REPO%/}"
UV_INSTALL_URL="${AGENT_CHAT_UV_INSTALL_URL:-https://astral.sh/uv/install.sh}"

# --------------------------------------------------------------------------- output

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-dumb}" != "dumb" ]; then
  C_BOLD=$'\033[1m' C_DIM=$'\033[2m' C_BLUE=$'\033[38;5;69m' C_AMBER=$'\033[38;5;214m' C_RED=$'\033[38;5;203m' C_OFF=$'\033[0m'
else
  C_BOLD="" C_DIM="" C_BLUE="" C_AMBER="" C_RED="" C_OFF=""
fi

# Output never stops the install: if the terminal or pipe goes away, the work still finishes
# (and the exit status still says how it went).
# shellcheck disable=SC2059  # out takes a printf format, like printf
out()  { printf "$@" 2>/dev/null || true; }
say()  { out '%s  ->%s %s\n' "$C_BLUE" "$C_OFF" "$*"; }
note() { out '      %s%s%s\n' "$C_DIM" "$*" "$C_OFF"; }
warn() { printf '%s  !! %s%s\n' "$C_AMBER" "$*" "$C_OFF" >&2 2>/dev/null || true; }
die()  { printf '%serror:%s %s\n' "$C_RED" "$C_OFF" "$*" >&2 2>/dev/null || true; exit 1; }

# A path for display, with $HOME shown as ~.
pretty() { case "$1" in "$HOME"/*) printf '~%s' "${1#"$HOME"}" ;; *) printf '%s' "$1" ;; esac; }

# Show the last lines of a log, indented, after a step failed.
show_tail() { [ -f "$1" ] && tail -n "${2:-15}" "$1" | sed 's/^/      | /' >&2 2>/dev/null || true; }

# --------------------------------------------------------------------------- helpers

need() { command -v "$1" >/dev/null 2>&1 || die "this installer needs '$1'. Install it and run the command again."; }

# A directory path without repeated or trailing slashes ("/" stays "/").
normdir() {
  local p="$1"
  while :; do case "$p" in *//*) p="${p//\/\//\/}" ;; *) break ;; esac; done
  [ "$p" = / ] || p="${p%/}"
  printf '%s' "$p"
}

HTTP_WARNED=0
# Sets PROTO_OPTS, the curl options for URL: https stays https through every redirect. Plain
# http is used only when an AGENT_CHAT_* override names an http address, with one warning.
proto_opts() {
  PROTO_OPTS=()
  case "$1" in
    https://*) PROTO_OPTS=(--proto '=https' --proto-redir '=https' --tlsv1.2) ;;
    http://*)
      if [ "$HTTP_WARNED" = 0 ]; then
        warn "downloading over plain http ($1), as an AGENT_CHAT_* setting asked"
        HTTP_WARNED=1
      fi ;;
  esac
}

fetch() {  # fetch URL FILE [quiet]: download, failing on HTTP errors
  local q=-sS
  if [ "${3:-}" = quiet ]; then q=-s; fi
  proto_opts "$1"
  curl -fL "$q" --retry 2 --connect-timeout 15 ${PROTO_OPTS[@]+"${PROTO_OPTS[@]}"} -o "$2" "$1"
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then openssl dgst -sha256 -r "$1" | awk '{print $1}'
  else return 1
  fi
}

find_uv() {
  local c
  if c=$(command -v uv 2>/dev/null); then printf '%s\n' "$c"; return 0; fi
  for c in ${UV_INSTALL_DIR:+"$UV_INSTALL_DIR/uv"} ${XDG_BIN_HOME:+"$XDG_BIN_HOME/uv"} "$HOME/.local/bin/uv" "$HOME/.cargo/bin/uv"; do
    if [ -x "$c" ]; then printf '%s\n' "$c"; return 0; fi
  done
  return 1
}

find_cargo() {
  local c
  if c=$(command -v cargo 2>/dev/null); then printf '%s\n' "$c"; return 0; fi
  if [ -x "$HOME/.cargo/bin/cargo" ]; then printf '%s\n' "$HOME/.cargo/bin/cargo"; return 0; fi
  return 1
}

# Is PID the Agent Chat server for PORT (and not some other process that reused the number)?
is_server_pid() {
  local cmd=""
  case "$1" in ''|*[!0-9]*) return 1 ;; esac
  kill -0 "$1" 2>/dev/null || return 1
  if [ -r "/proc/$1/cmdline" ]; then cmd=$(tr '\0' ' ' < "/proc/$1/cmdline" 2>/dev/null || true)
  else cmd="$(ps -p "$1" -o command= 2>/dev/null || true) "
  fi
  case "$cmd" in *"app.main:app "*"--port $2 "*) return 0 ;; esac
  return 1
}

# When process PID started, to tell it apart from a later process that got the same number.
proc_start() {
  local s
  local -a f
  if [ -r "/proc/$1/stat" ]; then
    s=$(cat "/proc/$1/stat" 2>/dev/null) || return 0
    read -r -a f <<< "${s##*) }"   # the fields after "pid (command name)"; starttime is the 20th
    printf '%s' "${f[19]:-}"
  else
    ps -o lstart= -p "$1" 2>/dev/null | tr -d ' ' || true
  fi
}

# Locks are symlinks: making one is atomic and fails if it exists, and its target names the
# owner as "pid:start". A lock whose owner is gone (killed install, reboot) is taken over.
lock_owner_alive() {
  local pid="${1%%:*}" start="${1#*:}" now
  case "$pid" in ''|*[!0-9]*) return 1 ;; esac
  kill -0 "$pid" 2>/dev/null || return 1
  [ -n "$start" ] || return 0
  now=$(proc_start "$pid")
  [ -z "$now" ] || [ "$now" = "$start" ]
}

# lock_take PATH: 0 = the lock is ours now; 1 = a live process holds it (pid in LOCK_HOLDER);
# 2 = it can't be created.
lock_take() {
  local path="$1" holder t i=0
  LOCK_ME="$$:$(proc_start $$)" LOCK_HOLDER=""
  while [ "$i" -lt 100 ]; do
    i=$((i + 1))
    if ln -s "$LOCK_ME" "$path" 2>/dev/null; then return 0; fi
    if ! holder=$(readlink "$path" 2>/dev/null); then
      if [ -e "$path" ] || [ -L "$path" ]; then return 2; fi
      continue   # released a moment ago: try again
    fi
    if lock_owner_alive "$holder"; then LOCK_HOLDER="${holder%%:*}"; return 1; fi
    # Stale. Remove it only if it is still that same stale lock; a second lock around the
    # check-and-remove keeps two installers that both saw it from removing each other's.
    if ln -s "$LOCK_ME" "$path.takeover" 2>/dev/null; then
      if [ "$(readlink "$path" 2>/dev/null || true)" = "$holder" ]; then rm -f "$path"; fi
      rm -f "$path.takeover"
    else
      t=$(readlink "$path.takeover" 2>/dev/null || true)
      if [ -n "$t" ] && ! lock_owner_alive "$t"; then rm -f "$path.takeover"; fi
      sleep 0.1
    fi
  done
  return 2
}

# The install root that an installed launcher belongs to (empty if it doesn't say).
launcher_home() { sed -n 's/^# install root: //p' "$1" 2>/dev/null | head -n 1; }

# A value from an install receipt (install.env).
receipt_get() {
  if [ -f "$1" ]; then awk -v k="$2=" 'index($0, k) == 1 { print substr($0, length(k) + 1); exit }' "$1"; fi
}

# --------------------------------------------------------------------------- steps

detect_platform() {
  OS=$(uname -s)
  ARCH=$(uname -m)
  case "$OS" in
    Linux) OS=linux ;;
    Darwin) OS=macos ;;
    *) die "Agent Chat installs on Linux and macOS; this is $OS. On Windows, run this inside WSL." ;;
  esac
  case "$ARCH" in
    x86_64|amd64) ARCH=x86_64 ;;
    aarch64|arm64) ARCH=aarch64 ;;
  esac
}

resolve_version() {
  local v="${AGENT_CHAT_VERSION:-}" url="" re='^v[0-9]+\.[0-9]+\.[0-9]+([.+-][0-9A-Za-z.+-]+)?$'
  local latest="${AGENT_CHAT_LATEST_URL:-$REPO/releases/latest}"
  if [ -n "$v" ]; then
    case "$v" in v*) ;; *) v="v$v" ;; esac
    [[ "$v" =~ $re ]] || die "AGENT_CHAT_VERSION=$AGENT_CHAT_VERSION is not a version like 0.3.0"
    TAG="$v"
    return
  fi
  # GitHub redirects releases/latest to releases/tag/<tag>: no API call, no token, no rate limit.
  proto_opts "$latest"
  url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' --connect-timeout 15 --max-time 30 \
          ${PROTO_OPTS[@]+"${PROTO_OPTS[@]}"} "$latest" 2>/dev/null) || url=""
  v="${url##*/tag/}"
  if [ "$v" != "$url" ] && [[ "$v" =~ $re ]]; then
    TAG="$v"
  else
    TAG="$PINNED_VERSION"
    warn "couldn't look up the latest release; installing $TAG"
  fi
}

# AGENT_CHAT_HOME gets app/, data/ and workspace/ and the uninstaller removes them again, so it
# has to be a folder of Agent Chat's own: refuse one that holds anything else.
check_home() {
  local f name
  case "$AC_HOME" in /) die "AGENT_CHAT_HOME can't be /" ;; esac
  [ "$AC_HOME" != "$(normdir "$HOME")" ] || die "AGENT_CHAT_HOME can't be your home folder itself; use a folder of its own, e.g. ~/agent-chat"
  [ -d "$AC_HOME" ] || return 0
  if [ ! -f "$AC_HOME/install.env" ]; then
    for f in "$AC_HOME"/* "$AC_HOME"/.[!.]* "$AC_HOME"/..?*; do
      [ -e "$f" ] || [ -L "$f" ] || continue
      name="${f##*/}"
      case "$name" in
        app|data|workspace|install.env|.install.env.new|.install-lock|.install-lock.takeover|.install.lock|.app-new|.app-old|rescued-*) ;;
        *) die "$(pretty "$AC_HOME") already holds other files ($name). Agent Chat needs a folder of its own, e.g. AGENT_CHAT_HOME=$(pretty "$AC_HOME")/agent-chat" ;;
      esac
    done
  fi
  check_code_dir
}

# An app/ folder that this installer didn't put there (a git checkout, say) is never replaced.
check_code_dir() {
  if [ -e "$CODE" ] && [ ! -f "$CODE/.agent-chat-installed" ]; then
    die "$(pretty "$CODE") exists but wasn't installed by this installer (a source checkout?), so it won't be replaced. Pick another AGENT_CHAT_HOME, or move that folder away."
  fi
}

# The launcher in BIN_DIR records which install it belongs to. If that is a different install
# (say XDG_DATA_HOME was set back then and isn't now), update that one instead of silently
# pointing the launcher at a new, empty install.
check_existing_launcher() {
  local l="$BIN_DIR/agent-chat" other
  [ -e "$l" ] || [ -L "$l" ] || return 0
  grep -q '^INSTALLED_HOME=' "$l" 2>/dev/null \
    || die "$(pretty "$l") exists and isn't the Agent Chat launcher; move it away or set AGENT_CHAT_BIN"
  other=$(launcher_home "$l")
  [ -n "$other" ] && [ "$other" != "$AC_HOME" ] && [ -f "$other/install.env" ] || return 0
  if [ -n "${AGENT_CHAT_HOME:-}" ]; then
    die "$(pretty "$l") belongs to the Agent Chat installed in $(pretty "$other"), not $(pretty "$AC_HOME"). To update that one, run this without AGENT_CHAT_HOME; to keep both, give this one its own AGENT_CHAT_BIN."
  fi
  note "updating the existing install in $(pretty "$other"), which $(pretty "$l") runs"
  AC_HOME="$other"
  CODE="$AC_HOME/app"
}

# Before anything changes: the launcher folder must take files and run them.
check_bin_dir() {
  local t="$BIN_DIR/.agent-chat-check.$$" rc=0
  if ! mkdir -p "$BIN_DIR" 2>/dev/null || ! { printf '#!/bin/sh\nexit 0\n' > "$t"; } 2>/dev/null; then
    die "can't write to $(pretty "$BIN_DIR"). Choose a folder you own (on your PATH) with AGENT_CHAT_BIN=..."
  fi
  chmod 755 "$t" && "$t" 2>/dev/null || rc=$?
  rm -f "$t"
  [ "$rc" = 0 ] || die "programs in $(pretty "$BIN_DIR") can't be run (is it mounted noexec?). Choose another folder on your PATH with AGENT_CHAT_BIN=..."
}

take_lock() {
  local rc=0 lock="$AC_HOME/.install-lock"
  lock_take "$lock" || rc=$?
  case "$rc" in
    0) LOCK="$lock" ;;   # set only once it's ours, so cleanup never removes someone else's
    1) die "another install or update is running (pid $LOCK_HOLDER); let it finish first. If none is running, delete $(pretty "$lock") and try again." ;;
    *) die "couldn't create the lock $(pretty "$lock")" ;;
  esac
}

# On any exit. If the old version was moved aside and the new one isn't finished, put the old
# one back, and restart what was stopped, so an interrupted update leaves a working install.
# Only ever removes what this run created.
cleanup() {
  trap - EXIT
  if [ -n "$SWAP_OLD" ] && [ -d "$SWAP_OLD" ]; then
    rm -rf "$CODE"
    mv "$SWAP_OLD" "$CODE"
    SWAP_OLD=""
    warn "the update didn't finish; the previous version is back in place"
  fi
  if [ "${#RESTART_PORTS[@]}" -gt 0 ] && [ -f "$CODE/bin/agent-chat" ]; then restart_servers; fi
  if [ -n "$TUI_NEW" ]; then rm -f "$TUI_NEW"; fi
  if [ -n "$STAGE" ]; then rm -rf "$STAGE"; fi
  if [ -n "$WORK" ]; then rm -rf "$WORK"; fi
  if [ -n "$LOCK" ] && [ "$(readlink "$LOCK" 2>/dev/null || true)" = "$LOCK_ME" ]; then rm -f "$LOCK"; fi
  if [ "$CREATED_HOME" = 1 ]; then rmdir "$AC_HOME" 2>/dev/null || true; fi   # a first install that failed
  return 0
}

download_source() {
  local url="${AGENT_CHAT_SRC_URL:-$REPO/archive/refs/tags/$TAG.tar.gz}"
  say "downloading Agent Chat $TAG"
  fetch "$url" "$WORK/src.tar.gz" || die "couldn't download $url"
  STAGE="$AC_HOME/.app-new"
  rm -rf "$STAGE"
  mkdir -p "$STAGE"
  tar --no-same-owner -xzf "$WORK/src.tar.gz" -C "$STAGE" --strip-components=1 || die "couldn't unpack $url"
  [ -f "$STAGE/app/main.py" ] && [ -f "$STAGE/pyproject.toml" ] && [ -f "$STAGE/uv.lock" ] \
    || die "$url doesn't look like Agent Chat (no app/main.py, pyproject.toml or uv.lock)"
  [ -f "$STAGE/bin/agent-chat" ] \
    || die "release $TAG has no launcher (bin/agent-chat): it predates this installer. Pick a newer one with AGENT_CHAT_VERSION."
  grep -qx 'INSTALLED_HOME=""' "$STAGE/bin/agent-chat" || die "the launcher in $TAG is not one this installer knows how to set up"
  APP_VERSION=$(sed -n 's/^VERSION = "\(.*\)"/\1/p' "$STAGE/app/main.py")
}

# Gets the terminal client into TUI_NEW, next to where it will live (so it is tested where it
# will run, not in a temp folder that may be noexec), or leaves TUI_NEW empty.
get_tui() {
  local got=0 msg rc=0
  TUI_HOW=""
  if [ "${AGENT_CHAT_NO_TUI:-}" = 1 ]; then
    note "terminal client skipped (AGENT_CHAT_NO_TUI=1)"
    return 0
  fi
  if [ "$OS-$ARCH" = "linux-x86_64" ] && [ "${AGENT_CHAT_BUILD_TUI:-}" != 1 ]; then
    if download_tui; then got=1; TUI_HOW="prebuilt"; fi
  fi
  if [ "$got" = 0 ]; then
    if find_cargo >/dev/null; then
      if build_tui; then got=1; TUI_HOW="built with cargo"; fi
    elif [ "$OS-$ARCH" != "linux-x86_64" ] || [ "${AGENT_CHAT_BUILD_TUI:-}" = 1 ]; then
      note "no prebuilt terminal client for $OS-$ARCH and no cargo to build one (https://rustup.rs);"
      note "the web UI (agent-chat web) has everything. Install Rust and update to add it later."
    fi
  fi
  [ "$got" = 1 ] || return 0
  TUI_NEW="$BIN_DIR/.agent-chat-tui.new.$$"
  cp "$WORK/agent-chat-tui" "$TUI_NEW"
  chmod 755 "$TUI_NEW"
  msg=$("$TUI_NEW" --version 2>&1) || rc=$?
  if [ "$rc" != 0 ]; then
    warn "the terminal client doesn't run on this system ($(printf '%s' "$msg" | head -n 1)); skipping it (use agent-chat web)"
    rm -f "$TUI_NEW"
    TUI_NEW="" TUI_HOW=""
  fi
}

download_tui() {
  local url="${AGENT_CHAT_TUI_URL:-$REPO/releases/download/$TAG/agent-chat-tui-linux-x86_64}"
  local sum_url="${AGENT_CHAT_TUI_SHA256_URL:-$url.sha256}" want got
  say "downloading the terminal client"
  if ! fetch "$url" "$WORK/agent-chat-tui" quiet; then
    warn "no prebuilt terminal client at $url"
    return 1
  fi
  if ! fetch "$sum_url" "$WORK/agent-chat-tui.sha256" quiet; then
    warn "no checksum at $sum_url, so the prebuilt terminal client can't be checked; not using it"
    return 1
  fi
  want=$(awk 'NR==1 {print $1}' "$WORK/agent-chat-tui.sha256" | tr -d '\r' | tr 'A-F' 'a-f')
  got=$(sha256_of "$WORK/agent-chat-tui" | tr 'A-F' 'a-f') || die "no sha256sum, shasum or openssl to check the download with"
  [[ "$want" =~ ^[0-9a-f]{64}$ ]] || die "the checksum file at $sum_url is malformed"
  if [ "$want" != "$got" ]; then
    die "checksum mismatch for $url (expected $want, got $got). Nothing was installed."
  fi
  note "checksum ok"
}

build_tui() {
  local cargo target
  cargo=$(find_cargo)
  target="${XDG_CACHE_HOME:-$HOME/.cache}/agent-chat/tui-target"
  say "building the terminal client with cargo (a few minutes the first time)"
  if ! CARGO_TARGET_DIR="$target" "$cargo" build --release --locked --manifest-path "$STAGE/tui/Cargo.toml" \
        >"$WORK/cargo.log" 2>&1; then
    warn "cargo build failed; continuing without the terminal client (agent-chat web works). Last lines:"
    show_tail "$WORK/cargo.log"
    return 1
  fi
  cp "$target/release/agent-chat-tui" "$WORK/agent-chat-tui"
}

ensure_uv() {
  if UV=$(find_uv); then return 0; fi
  if [ "${AGENT_CHAT_NO_UV:-}" = 1 ]; then
    die "uv isn't installed, and AGENT_CHAT_NO_UV=1 says not to install it. Install uv (https://docs.astral.sh/uv/) and run this again."
  fi
  say "installing uv, which runs the Python side ($UV_INSTALL_URL)"
  fetch "$UV_INSTALL_URL" "$WORK/uv-install.sh" || die "couldn't download $UV_INSTALL_URL"
  # Agent Chat finds uv on its own, so leave the shell startup files alone.
  if ! UV_NO_MODIFY_PATH=1 sh "$WORK/uv-install.sh" >"$WORK/uv-install.log" 2>&1; then
    show_tail "$WORK/uv-install.log"
    die "installing uv failed. Install it yourself (https://docs.astral.sh/uv/) and run this again."
  fi
  UV=$(find_uv) || die "uv was installed but can't be found; add ~/.local/bin to PATH and run this again"
  UV_OURS=1
}

stop_pid() {  # TERM, then up to 15 s for a clean exit (the app ends its streams within ~2 s)
  local i=0
  kill -TERM "$1" 2>/dev/null || true
  while kill -0 "$1" 2>/dev/null && [ "$i" -lt 75 ]; do sleep 0.2; i=$((i + 1)); done
  if kill -0 "$1" 2>/dev/null; then kill -KILL -- "-$1" 2>/dev/null || kill -KILL "$1" 2>/dev/null || true; fi
}

# Stop the servers the launcher started from this install, so the code can be swapped under
# them. They come back on the new version at the end, with the folders they were using.
stop_managed_servers() {
  local f pid port code data ws
  for f in "$STATE_DIR"/server-*.pid; do
    [ -f "$f" ] || continue
    pid="" port="" code="" data="" ws=""
    { IFS= read -r pid; IFS= read -r port; IFS= read -r code; IFS= read -r data; IFS= read -r ws; } < "$f" || true
    [ "$code" = "$CODE" ] || continue
    is_server_pid "$pid" "$port" || continue
    say "stopping Agent Chat on port $port for the update"
    stop_pid "$pid"
    rm -f "$f"
    RESTART_PORTS+=("$port") RESTART_DATA+=("$data") RESTART_WS+=("$ws")
  done
}

restart_servers() {
  local i=0
  while [ "$i" -lt "${#RESTART_PORTS[@]}" ]; do
    if AGENT_CHAT_INSTALLING=1 AGENT_CHAT_PORT="${RESTART_PORTS[$i]}" AGENT_CHAT_STATE="$STATE_DIR" \
         AGENT_CHAT_DATA="${RESTART_DATA[$i]}" AGENT_CHAT_WORKSPACE="${RESTART_WS[$i]}" \
         bash "$CODE/bin/agent-chat" start </dev/null >/dev/null 2>&1; then
      say "restarted Agent Chat on port ${RESTART_PORTS[$i]}"
    else
      warn "couldn't restart Agent Chat on port ${RESTART_PORTS[$i]}: run agent-chat start"
    fi
    i=$((i + 1))
  done
  RESTART_PORTS=() RESTART_DATA=() RESTART_WS=()
}

# data/ or workspace/ inside the code folder means someone ran the server from there without
# AGENT_CHAT_DATA (e.g. run.sh). Never let an update delete that: move it out first.
rescue_from_old_code() {
  local d dest=""
  for d in data workspace; do
    if [ -d "$CODE/$d" ] && [ -n "$(ls -A "$CODE/$d" 2>/dev/null)" ]; then
      [ -n "$dest" ] || { dest="$AC_HOME/rescued-$(date +%Y%m%d-%H%M%S)"; mkdir -p "$dest"; }
      mv "$CODE/$d" "$dest/$d"
      warn "found $d/ inside the app folder; moved it to $(pretty "$dest/$d") so the update can't delete it"
    fi
  done
}

# Put the new code in place: rename the old folder away, rename the new one in, set up its
# Python environment, and only then drop the old one. If uv sync fails, the old version comes back.
swap_and_sync() {
  local old="$AC_HOME/.app-old"
  check_code_dir
  printf '%s\n' "$TAG" > "$STAGE/.agent-chat-installed"   # marked before it becomes app/
  rm -rf "$old"
  if [ -d "$CODE" ]; then
    rescue_from_old_code
    mv "$CODE" "$old"
    SWAP_OLD="$old"   # until finish_swap, any exit puts this back (see cleanup)
  fi
  mv "$STAGE" "$CODE"
  STAGE=""

  if [ -d "$old/.venv" ]; then say "updating the Python environment (uv sync)"
  else say "setting up the Python environment (uv sync; a minute or so the first time)"
  fi
  if ! (cd "$CODE" && unset VIRTUAL_ENV && "$UV" sync --frozen) >"$WORK/uv-sync.log" 2>&1; then
    show_tail "$WORK/uv-sync.log" 20
    die "uv sync failed (log above). Run this installer again once that's fixed."
  fi
}

finish_swap() {
  if [ -n "$SWAP_OLD" ]; then rm -rf "$SWAP_OLD"; SWAP_OLD=""; fi
}

make_private_dir() {  # chats, memory and the workspace are for this user only
  if [ ! -d "$1" ]; then mkdir -p "$1" && chmod 700 "$1"; fi
}

install_launcher() {
  local line tmp="$BIN_DIR/.agent-chat.new.$$"
  # Copy the launcher, recording where this install lives so it works without any env vars.
  while IFS= read -r line || [ -n "$line" ]; do
    if [ "$line" = 'INSTALLED_HOME=""' ]; then printf 'INSTALLED_HOME=%q\n# install root: %s\n' "$AC_HOME" "$AC_HOME"
    else printf '%s\n' "$line"
    fi
  done < "$CODE/bin/agent-chat" > "$tmp"
  chmod 755 "$tmp"
  mv -f "$tmp" "$BIN_DIR/agent-chat"   # rename, so a launcher that is running keeps its old copy
  if [ -n "$TUI_NEW" ]; then
    mv -f "$TUI_NEW" "$BIN_DIR/agent-chat-tui"
    TUI_NEW=""
    TUI_PATH="$BIN_DIR/agent-chat-tui"
  elif [ -x "$BIN_DIR/agent-chat-tui" ]; then
    TUI_PATH="$BIN_DIR/agent-chat-tui"
    note "kept the terminal client that was already installed"
  else
    TUI_PATH=""
  fi
}

write_receipt() {
  local tmp="$AC_HOME/.install.env.new"
  {
    echo "# Written by the Agent Chat installer; read by the agent-chat launcher."
    echo "VERSION=$TAG"
    echo "SITE=$SITE"
    echo "BIN_DIR=$BIN_DIR"
    echo "UV=$UV"
    echo "UV_INSTALLED_BY_INSTALLER=$UV_OURS"
    echo "TUI=$TUI_PATH"
    echo "INSTALLED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  } > "$tmp"
  mv -f "$tmp" "$AC_HOME/install.env"
}

# shellcheck disable=SC2088  # the ~ paths are printed for the user, not used here
path_hint() {
  local p shown="$BIN_DIR" sh rc=""
  p=$(printf '%s' "$PATH" | sed 's#//*:#:#g; s#//*$##')
  case ":$p:" in *":$BIN_DIR:"*) return 0 ;; esac
  case "$BIN_DIR" in "$HOME"/*) shown="\$HOME${BIN_DIR#"$HOME"}" ;; esac
  out '\n'
  warn "$(pretty "$BIN_DIR") is not on your PATH yet. Add it, then open a new terminal:"
  sh=$(basename "${SHELL:-sh}")
  case "$BIN_DIR" in *[\'\"\\\$\`]*) sh=unknown ;; esac   # no copy-paste line for odd characters
  case "$sh" in
    fish) out "      fish_add_path '%s'\n" "$BIN_DIR" ;;
    zsh) rc="~/.zshrc" ;;
    bash) rc="~/.bashrc"; if [ "$OS" = macos ]; then rc="~/.bash_profile"; fi ;;
    sh|dash|ash|ksh|mksh) rc="~/.profile" ;;
    csh|tcsh)
      rc="~/.cshrc"; if [ "$sh" = tcsh ] && [ -f "$HOME/.tcshrc" ]; then rc="~/.tcshrc"; fi
      # shellcheck disable=SC2016  # $path is meant literally: it goes into the startup file
      out '      echo '"'"'set path = ("%s" $path)'"'"' >> %s\n' "$shown" "$rc"
      return 0 ;;
    *) out "      add %s to PATH in your shell's startup file\n" "$BIN_DIR"; return 0 ;;
  esac
  # shellcheck disable=SC2016  # $PATH is meant literally: it goes into the startup file
  if [ -n "$rc" ]; then out '      echo '"'"'export PATH="%s:$PATH"'"'"' >> %s\n' "$shown" "$rc"; fi
}

# --------------------------------------------------------------------------- main

main() {
  # Every global this script removes or restores starts empty, so nothing from the caller's
  # environment (WORK, STAGE, LOCK, ...) can point cleanup at their files.
  WORK="" STAGE="" LOCK="" LOCK_ME="" LOCK_HOLDER="" SWAP_OLD="" TUI_NEW="" TUI_HOW="" TUI_PATH=""
  APP_VERSION="" UV="" UV_OURS=0 TAG="" OS="" ARCH="" CREATED_HOME=0
  RESTART_PORTS=() RESTART_DATA=() RESTART_WS=()

  exec </dev/null   # piped from curl: nothing below may read the rest of the script as input
  [ -n "${HOME:-}" ] || die "HOME is not set"
  if [ "${EUID:-$(id -u)}" = 0 ] && [ "${AGENT_CHAT_ALLOW_ROOT:-}" != 1 ]; then
    die "don't run this as root or with sudo: Agent Chat installs for your own user, under ~/.local. (To install it for root anyway, set AGENT_CHAT_ALLOW_ROOT=1.)"
  fi
  need curl; need tar; need gzip; need mktemp; need awk; need sed
  detect_platform

  local data_root="${XDG_DATA_HOME:-$HOME/.local/share}" state_root="${XDG_STATE_HOME:-$HOME/.local/state}"
  case "$data_root" in /*) ;; *) data_root="$HOME/.local/share" ;; esac
  case "$state_root" in /*) ;; *) state_root="$HOME/.local/state" ;; esac
  AC_HOME=$(normdir "${AGENT_CHAT_HOME:-$data_root/agent-chat}")
  BIN_DIR=$(normdir "${AGENT_CHAT_BIN:-$HOME/.local/bin}")
  STATE_DIR=$(normdir "${AGENT_CHAT_STATE:-$state_root/agent-chat}")
  case "$AC_HOME" in /*) ;; *) die "AGENT_CHAT_HOME must be an absolute path" ;; esac
  case "$BIN_DIR" in /*) ;; *) die "AGENT_CHAT_BIN must be an absolute path" ;; esac
  case "/$AC_HOME/$BIN_DIR/" in */./*|*/../*) die "AGENT_CHAT_HOME and AGENT_CHAT_BIN can't contain . or .. parts" ;; esac
  case "$AC_HOME$BIN_DIR$STATE_DIR" in *$'\n'*) die "install paths can't contain line breaks" ;; esac
  CODE="$AC_HOME/app"
  check_existing_launcher
  check_home

  local previous=""
  previous=$(receipt_get "$AC_HOME/install.env" VERSION)
  if [ "$(receipt_get "$AC_HOME/install.env" UV_INSTALLED_BY_INSTALLER)" = 1 ]; then UV_OURS=1; fi

  resolve_version
  if [ -n "$previous" ] && [ "$previous" != "$TAG" ]; then
    out '%sAgent Chat%s  updating %s -> %s  (%s-%s)\n' "$C_BOLD" "$C_OFF" "$previous" "$TAG" "$OS" "$ARCH"
  elif [ -n "$previous" ]; then
    out '%sAgent Chat%s  reinstalling %s  (%s-%s)\n' "$C_BOLD" "$C_OFF" "$TAG" "$OS" "$ARCH"
  else
    out '%sAgent Chat%s  installing %s  (%s-%s)\n' "$C_BOLD" "$C_OFF" "$TAG" "$OS" "$ARCH"
  fi

  check_bin_dir
  if [ ! -d "$AC_HOME" ]; then CREATED_HOME=1; fi
  make_private_dir "$AC_HOME" 2>/dev/null || die "can't create $(pretty "$AC_HOME")"
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  trap '' PIPE   # a closed output pipe must not kill the install halfway (output just stops)
  take_lock
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/agent-chat-install.XXXXXX") || die "couldn't create a temporary folder in ${TMPDIR:-/tmp}"

  download_source
  get_tui           # verified before anything else changes: a bad download leaves no trace
  ensure_uv
  stop_managed_servers
  swap_and_sync
  make_private_dir "$AC_HOME/data"
  make_private_dir "$AC_HOME/workspace"
  install_launcher
  write_receipt
  finish_swap
  say "launcher: $(pretty "$BIN_DIR/agent-chat")${TUI_PATH:+   terminal client: $(pretty "$TUI_PATH")${TUI_HOW:+ ($TUI_HOW)}}"
  note "your chats and settings: $(pretty "$AC_HOME/data")   workspace: $(pretty "$AC_HOME/workspace")"

  restart_servers
  if command -v systemctl >/dev/null 2>&1 && systemctl --user is-active --quiet agent-chat.service 2>/dev/null; then
    note "the agent-chat systemd service is running: systemctl --user restart agent-chat loads this version"
  fi

  out '\n%sAgent Chat %s is installed.%s\n' "$C_BOLD" "${APP_VERSION:-$TAG}" "$C_OFF"
  path_hint
  local run="agent-chat" p
  p=$(printf '%s' "$PATH" | sed 's#//*:#:#g; s#//*$##')
  case ":$p:" in *":$BIN_DIR:"*) ;; *) run="$(pretty "$BIN_DIR")/agent-chat" ;; esac
  out '\n%sNext:%s\n' "$C_BOLD" "$C_OFF"
  out '  1. start your model server, e.g.  %sllama-server -m model.gguf --jinja --port 8080%s\n' "$C_BLUE" "$C_OFF"
  if [ -n "$TUI_PATH" ]; then
    out '  2. run  %s%s%s  (terminal)  or  %s%s web%s  (browser)\n' "$C_BLUE" "$run" "$C_OFF" "$C_BLUE" "$run" "$C_OFF"
  else
    out '  2. run  %s%s web%s  to open Agent Chat in your browser\n' "$C_BLUE" "$run" "$C_OFF"
  fi
  return 0
}

main "$@"

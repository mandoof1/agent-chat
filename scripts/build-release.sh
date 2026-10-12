#!/usr/bin/env bash
# Build the terminal client the installer downloads, for the machine this runs on:
#   dist/agent-chat-tui-<os>-<arch>          linux: static (musl); macos: native; stripped
#   dist/agent-chat-tui-<os>-<arch>.sha256   its checksum, in sha256sum format
# <os>-<arch> is linux-x86_64, linux-aarch64, macos-aarch64 or macos-x86_64, the names the
# installer asks for. On macOS, TARGET=x86_64-apple-darwin builds the Intel one on Apple silicon.
#
#   scripts/build-release.sh [CARGO_TARGET_DIR]
#
# The target dir defaults to $CARGO_TARGET_DIR, else tui/target-release, so this build never
# waits on (or invalidates) the debug/release builds you run in tui/ while developing.
# Releases are built by .github/workflows/release.yml when a vX.Y.Z tag is pushed
# (X.Y.Z = VERSION in app/main.py); this script is what it runs on each platform.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
DIST="${DIST:-$ROOT/dist}"
export CARGO_TARGET_DIR="${1:-${CARGO_TARGET_DIR:-$ROOT/tui/target-release}}"
export PATH="$HOME/.cargo/bin:$PATH"

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi; }

case "$(uname -s)" in
  Linux) OS=linux; TARGET="${TARGET:-$(uname -m)-unknown-linux-musl}" ;;
  Darwin) OS=macos; TARGET="${TARGET:-$(uname -m | sed 's/^arm64$/aarch64/')-apple-darwin}" ;;
  *) die "build this on Linux or macOS" ;;
esac
ARCH="${TARGET%%-*}"
case "$ARCH" in x86_64|aarch64) ;; *) die "no release build for $TARGET" ;; esac
NAME="agent-chat-tui-$OS-$ARCH"

command -v cargo >/dev/null || die "cargo not found (install Rust from https://rustup.rs)"
if command -v rustup >/dev/null; then
  say "rustup target add $TARGET"
  rustup target add "$TARGET" >/dev/null
fi

if [[ "$OS" == linux ]]; then
  # The TLS stack (ring) compiles a little C. Without a musl cross gcc, clang (or plain cc) builds
  # it fine for the musl target, since that code needs no libc headers beyond the compiler's own.
  t="${TARGET//-/_}"
  cc_var="CC_$t" ar_var="AR_$t"
  if [[ -z "${!cc_var:-}" ]] && ! command -v "$ARCH-linux-musl-gcc" >/dev/null; then
    for c in musl-gcc clang cc; do
      if command -v "$c" >/dev/null; then export "$cc_var=$c"; break; fi
    done
  fi
  if [[ -z "${!ar_var:-}" ]] && ! command -v "$ARCH-linux-musl-ar" >/dev/null; then
    for a in llvm-ar ar; do
      if command -v "$a" >/dev/null; then export "$ar_var=$a"; break; fi
    done
  fi
fi

say "cargo build --release --target $TARGET  (target dir: $CARGO_TARGET_DIR)"
# Strip symbols at link time: no binutils needed and the checksum matches what ships.
(cd "$ROOT/tui" && CARGO_PROFILE_RELEASE_STRIP=symbols cargo build --release --locked --target "$TARGET")

BIN="$CARGO_TARGET_DIR/$TARGET/release/agent-chat-tui"
[[ -x "$BIN" ]] || die "build finished but $BIN is missing"

mkdir -p "$DIST"
cp "$BIN" "$DIST/$NAME.tmp"
if [[ "$OS" == linux ]] && command -v strip >/dev/null; then strip "$DIST/$NAME.tmp" 2>/dev/null || true; fi
chmod 755 "$DIST/$NAME.tmp"
mv -f "$DIST/$NAME.tmp" "$DIST/$NAME"
(cd "$DIST" && sha256 "$NAME" > "$NAME.sha256")

# A release binary that needs a library the user may not have would fail on their machine:
# refuse to ship it. Linux: fully static. macOS: only the system's own libraries.
if [[ "$OS" == linux ]]; then
  info=$(file -b "$DIST/$NAME")
  case "$info" in
    *"statically linked"*|*"static-pie linked"*) ;;
    *) die "not a static binary: $info" ;;
  esac
  kind=static
else
  extra=$(otool -L "$DIST/$NAME" | tail -n +2 | awk '{print $1}' | grep -v -E '^(/usr/lib/|/System/Library/)' || true)
  [[ -z "$extra" ]] || die "links libraries outside the system: $extra"
  kind="system libraries only"
fi
# Run it where this machine can: its own architecture, or an Intel build under Rosetta.
if [[ "$ARCH" == "$(uname -m | sed 's/^arm64$/aarch64/')" ]] || { [[ "$OS" == macos ]] && arch -x86_64 /usr/bin/true 2>/dev/null; }; then
  "$DIST/$NAME" --version >/dev/null || die "the built binary does not run"
fi

VERSION=$(sed -n 's/^VERSION = "\(.*\)"/\1/p' "$ROOT/app/main.py")
say "built $NAME $VERSION: $(du -h "$DIST/$NAME" | cut -f1), $kind, stripped"
cat "$DIST/$NAME.sha256"
# The installer falls back to a pinned release when it can't look up the latest one.
PINNED=$(sed -n 's/^PINNED_VERSION="\(.*\)"/\1/p' "$ROOT/site/install.sh")
if [[ "$PINNED" != "v$VERSION" ]]; then
  printf 'warning: site/install.sh has PINNED_VERSION="%s" but the app is v%s (app/main.py): make them agree before publishing.\n' "$PINNED" "$VERSION" >&2
fi
# The AGENT_CHAT_VERSION examples on the page and in the installer must name a release that exists.
for f in site/install.sh site/index.html; do
  ex=$(grep -o 'AGENT_CHAT_VERSION=[0-9][0-9.]*[0-9]' "$ROOT/$f" | head -n 1 || true)
  ex="${ex#AGENT_CHAT_VERSION=}"
  if [[ -n "$ex" && "v$ex" != "$PINNED" ]]; then
    printf 'warning: %s gives AGENT_CHAT_VERSION=%s as its example, but PINNED_VERSION is %s.\n' "$f" "$ex" "$PINNED" >&2
  fi
done

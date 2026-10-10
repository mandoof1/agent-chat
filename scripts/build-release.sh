#!/usr/bin/env bash
# Build the release assets the installer downloads:
#   dist/agent-chat-tui-linux-x86_64         static (musl) terminal client, stripped
#   dist/agent-chat-tui-linux-x86_64.sha256  its checksum, in sha256sum format
#
#   scripts/build-release.sh [CARGO_TARGET_DIR]
#
# The target dir defaults to $CARGO_TARGET_DIR, else tui/target-release, so this build never
# waits on (or invalidates) the debug/release builds you run in tui/ while developing.
# Then attach both files to the GitHub release vX.Y.Z (X.Y.Z = VERSION in app/main.py).
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TARGET=x86_64-unknown-linux-musl
NAME=agent-chat-tui-linux-x86_64
DIST="${DIST:-$ROOT/dist}"
export CARGO_TARGET_DIR="${1:-${CARGO_TARGET_DIR:-$ROOT/tui/target-release}}"
export PATH="$HOME/.cargo/bin:$PATH"

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[[ "$(uname -s)-$(uname -m)" == "Linux-x86_64" ]] || die "build this on Linux x86_64"
command -v cargo >/dev/null || die "cargo not found (install Rust from https://rustup.rs)"
if command -v rustup >/dev/null; then
  say "rustup target add $TARGET"
  rustup target add "$TARGET" >/dev/null
fi

# The TLS stack (ring) compiles a little C. Without a musl cross gcc, clang (or plain cc) builds
# it fine for the musl target, since that code needs no libc headers beyond the compiler's own.
if [[ -z "${CC_x86_64_unknown_linux_musl:-}" ]] && ! command -v x86_64-linux-musl-gcc >/dev/null; then
  for c in musl-gcc clang cc; do
    if command -v "$c" >/dev/null; then export CC_x86_64_unknown_linux_musl="$c"; break; fi
  done
fi
if [[ -z "${AR_x86_64_unknown_linux_musl:-}" ]] && ! command -v x86_64-linux-musl-ar >/dev/null; then
  for a in llvm-ar ar; do
    if command -v "$a" >/dev/null; then export AR_x86_64_unknown_linux_musl="$a"; break; fi
  done
fi

say "cargo build --release --target $TARGET  (target dir: $CARGO_TARGET_DIR)"
# Strip symbols at link time: no binutils needed and the checksum matches what ships.
(cd "$ROOT/tui" && CARGO_PROFILE_RELEASE_STRIP=symbols cargo build --release --locked --target "$TARGET")

BIN="$CARGO_TARGET_DIR/$TARGET/release/agent-chat-tui"
[[ -x "$BIN" ]] || die "build finished but $BIN is missing"

mkdir -p "$DIST"
cp "$BIN" "$DIST/$NAME.tmp"
if command -v strip >/dev/null; then strip "$DIST/$NAME.tmp" 2>/dev/null || true; fi
chmod 755 "$DIST/$NAME.tmp"
mv -f "$DIST/$NAME.tmp" "$DIST/$NAME"
(cd "$DIST" && sha256sum "$NAME" > "$NAME.sha256")

# A release binary that needs a shared library would fail on other distros: refuse to ship it.
info=$(file -b "$DIST/$NAME")
case "$info" in
  *"statically linked"*|*"static-pie linked"*) ;;
  *) die "not a static binary: $info" ;;
esac
"$DIST/$NAME" --version >/dev/null || die "the built binary does not run"

VERSION=$(sed -n 's/^VERSION = "\(.*\)"/\1/p' "$ROOT/app/main.py")
say "built $("$DIST/$NAME" --version): $(du -h "$DIST/$NAME" | cut -f1), static, stripped"
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
say "next: attach both files to release v$VERSION, e.g."
printf '    gh release create v%s %s/%s %s/%s.sha256\n' "$VERSION" "$DIST" "$NAME" "$DIST" "$NAME"

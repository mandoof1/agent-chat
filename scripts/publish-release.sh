#!/usr/bin/env bash
# Publish release vX.Y.Z from your own GitHub account (gh), with the terminal client for every
# platform as built by .github/workflows/release.yml for that tag.
#
#   git tag vX.Y.Z && git push origin vX.Y.Z      # starts the build
#   scripts/publish-release.sh vX.Y.Z             # waits for it, checks the files, publishes
#
# The release notes are the vX.Y.Z section of CHANGELOG.md.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TAG="${1:?usage: scripts/publish-release.sh vX.Y.Z}"
PLATFORMS=(linux-x86_64 linux-aarch64 macos-aarch64 macos-x86_64)
OUT="$ROOT/dist/release-$TAG"

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
sha256() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "$TAG is not a tag like v1.2.3"
git -C "$ROOT" ls-remote --exit-code --tags origin "refs/tags/$TAG" >/dev/null || die "$TAG isn't on origin: git push origin $TAG"
! gh release view "$TAG" >/dev/null 2>&1 || die "release $TAG already exists"
notes=$(awk -v v="${TAG#v}" '$0 ~ "^## " v "( |$)" {f=1; next} /^## / {if (f) exit} f' "$ROOT/CHANGELOG.md")
[[ -n "${notes//[[:space:]]/}" ]] || die "CHANGELOG.md has no section for ${TAG#v}"

say "waiting for the release build of $TAG"
run=""
for _ in $(seq 1 30); do
  run=$(gh run list --workflow release.yml --branch "$TAG" --limit 1 --json databaseId -q '.[0].databaseId' 2>/dev/null || true)
  [[ -n "$run" ]] && break
  sleep 10
done
[[ -n "$run" ]] || die "no release.yml run for $TAG (was the tag pushed?)"
gh run watch "$run" --exit-status >/dev/null || die "the build failed: gh run view $run --log-failed"

say "downloading and checking the binaries"
rm -rf "$OUT" && mkdir -p "$OUT"
gh run download "$run" --dir "$OUT"
files=()
for p in "${PLATFORMS[@]}"; do
  f="$OUT/agent-chat-tui-$p/agent-chat-tui-$p"
  [[ -f "$f" && -f "$f.sha256" ]] || die "the build has no agent-chat-tui-$p"
  (cd "$(dirname "$f")" && sha256 -c "agent-chat-tui-$p.sha256" >/dev/null) || die "checksum mismatch for agent-chat-tui-$p"
  files+=("$f" "$f.sha256")
done

say "publishing $TAG"
gh release create "$TAG" "${files[@]}" --verify-tag --title "${TAG#v}" --notes "$notes"
gh release view "$TAG" --json url,assets -q '"\(.url)\n\(.assets | map("  " + .name) | join("\n"))"'

#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SOURCE=${1:-$ROOT/refs/plainfeed-data-fixture}
TEMPORARY=$(mktemp -d "${TMPDIR:-/tmp}/plainfeed-local-recovery.XXXXXX")
REPOSITORY="$TEMPORARY/repository"

cleanup() {
  rm -rf "$TEMPORARY"
}
trap cleanup EXIT INT TERM

mkdir -p "$REPOSITORY"
cp -R "$SOURCE/." "$REPOSITORY/"
mkdir -p "$REPOSITORY/.plainfeed/backup/activation-interrupted"
mkdir -p "$REPOSITORY/.plainfeed/update.lock"
mv "$REPOSITORY/content" \
  "$REPOSITORY/.plainfeed/backup/activation-interrupted/content"
mv "$REPOSITORY/config" \
  "$REPOSITORY/.plainfeed/backup/activation-interrupted/config"
mkdir -p "$REPOSITORY/content"
SOURCE_ENTRY=$(find \
  "$REPOSITORY/.plainfeed/backup/activation-interrupted/content" \
  -type f -name '*.md' | head -n 1)
cp "$SOURCE_ENTRY" "$REPOSITORY/content/partial.md"

RECOVERY=$(wasmtime run \
  --dir "$REPOSITORY::/data" \
  "$ROOT/target/wasm32-wasip2/debug/plainfeed-sync.wasm" recover-local)
case "$RECOVERY" in
  *"local_recovery=completed"*) ;;
  *) echo "interrupted activation recovery did not complete" >&2; exit 1 ;;
esac
test ! -d "$REPOSITORY/.plainfeed/update.lock"
test ! -d "$REPOSITORY/.plainfeed/backup"
git -C "$REPOSITORY" diff --quiet
git -C "$REPOSITORY" diff --cached --quiet
git -C "$REPOSITORY" fsck --full >/dev/null

echo "Plainfeed recovered an interrupted local activation under Wasmtime"

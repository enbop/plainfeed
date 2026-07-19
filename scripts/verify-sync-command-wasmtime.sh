#!/bin/sh
set -eu

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: $0 PAT_FILE DATA_CHECKOUT [EXPECTED_CHECKOUT]" >&2
  exit 2
fi

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
PAT_FILE=$1
DATA_CHECKOUT=$2
EXPECTED_CHECKOUT=${3:-$DATA_CHECKOUT}
: "${PLAINFEED_TEST_REMOTE_URL:?set PLAINFEED_TEST_REMOTE_URL to the HTTPS test repository}"
REMOTE=$PLAINFEED_TEST_REMOTE_URL
WASM="$ROOT/target/wasm32-wasip2/debug/plainfeed-sync.wasm"
TEMPORARY=$(mktemp -d /tmp/plainfeed-sync-command.XXXXXX)
REPOSITORY="$TEMPORARY/repository"
LOG="$TEMPORARY/sync.log"

cleanup() {
  unset PLAINFEED_GITHUB_TOKEN PLAINFEED_REMOTE_URL
  rm -rf "$TEMPORARY"
}
trap cleanup EXIT INT TERM

mkdir -p "$REPOSITORY"
cp -R "$DATA_CHECKOUT/." "$REPOSITORY/"
PLAINFEED_GITHUB_TOKEN=$(tr -d '\r\n' <"$PAT_FILE")
PLAINFEED_REMOTE_URL=$REMOTE
export PLAINFEED_GITHUB_TOKEN PLAINFEED_REMOTE_URL

run_network_command() {
  wasmtime run \
    --env PLAINFEED_GITHUB_TOKEN \
    --env PLAINFEED_REMOTE_URL \
    -S inherit-network=y \
    -S allow-ip-name-lookup=y \
    --dir "$REPOSITORY::/data" \
    "$WASM" "$1"
}

FIRST=$(run_network_command force)
case "$FIRST" in
  *"pull=completed"*) ;;
  *) echo "forced pull did not complete" >&2; exit 1 ;;
esac
test -f "$REPOSITORY/.plainfeed/sync.toml"
grep -q "^remote_url = \"$REMOTE\"$" "$REPOSITORY/.plainfeed/sync.toml"
EXPECTED_TIP=$(git -C "$EXPECTED_CHECKOUT" rev-parse origin/main)
test "$(git -C "$REPOSITORY" rev-parse refs/heads/main)" = "$EXPECTED_TIP"
git -C "$REPOSITORY" diff --quiet
git -C "$REPOSITORY" diff --cached --quiet

PLAINFEED_REMOTE_URL=https://127.0.0.1:1/offline.git
export PLAINFEED_REMOTE_URL
TICK=$(run_network_command tick)
case "$TICK" in
  *"pull=not-due"*) ;;
  *) echo "immediate tick unexpectedly performed network synchronization" >&2; exit 1 ;;
esac

STATUS=$(wasmtime run \
  --dir "$REPOSITORY::/data" \
  "$WASM" status)
case "$STATUS" in
  *"format=plainfeed.sync-status/v1"*"last_remote_oid="*) ;;
  *) echo "status output is incomplete" >&2; exit 1 ;;
esac

BEFORE=$(git -C "$REPOSITORY" rev-parse refs/heads/main)
if run_network_command force >"$LOG" 2>&1; then
  echo "offline forced pull unexpectedly succeeded" >&2
  exit 1
fi
AFTER=$(git -C "$REPOSITORY" rev-parse refs/heads/main)
test "$BEFORE" = "$AFTER"
git -C "$REPOSITORY" diff --quiet
git -C "$REPOSITORY" diff --cached --quiet
grep -q '^last_error = ' "$REPOSITORY/.plainfeed/sync.toml"
if grep -Fq "$PLAINFEED_GITHUB_TOKEN" "$LOG"; then
  echo "offline error output leaked credentials" >&2
  exit 1
fi

PLAINFEED_REMOTE_URL=$REMOTE
export PLAINFEED_REMOTE_URL
SECOND=$(run_network_command force)
case "$SECOND" in
  *"pull=completed"*) ;;
  *) echo "recovery pull did not complete" >&2; exit 1 ;;
esac
if grep -q '^last_error = ' "$REPOSITORY/.plainfeed/sync.toml"; then
  echo "successful recovery did not clear last_error" >&2
  exit 1
fi

SOURCE_ENTRY=$(find "$REPOSITORY/content" -type f -name '*.md' | head -n 1)
cp "$SOURCE_ENTRY" "$REPOSITORY/content/plainfeed-local-conflict.md"
if run_network_command force >"$LOG" 2>&1; then
  echo "forced sync unexpectedly overwrote a local content change" >&2
  exit 1
fi
test -f "$REPOSITORY/.plainfeed/conflict.toml"
CONFLICT_STATUS=$(wasmtime run \
  --dir "$REPOSITORY::/data" \
  "$WASM" status)
case "$CONFLICT_STATUS" in
  *"conflict_active=true"*"content/plainfeed-local-conflict.md"*) ;;
  *) echo "status did not expose the local ownership conflict" >&2; exit 1 ;;
esac
rm "$REPOSITORY/content/plainfeed-local-conflict.md"
if run_network_command force >"$LOG" 2>&1; then
  echo "forced sync unexpectedly bypassed an unacknowledged conflict" >&2
  exit 1
fi
wasmtime run \
  --dir "$REPOSITORY::/data" \
  "$WASM" acknowledge-conflict >/dev/null
RECOVERED=$(run_network_command force)
case "$RECOVERED" in
  *"pull=completed"*) ;;
  *) echo "sync did not recover after removing the local conflict" >&2; exit 1 ;;
esac
test ! -f "$REPOSITORY/.plainfeed/conflict.toml"

mkdir -p "$REPOSITORY/.plainfeed/backup/activation-interrupted"
mkdir -p "$REPOSITORY/.plainfeed/update.lock"
mv "$REPOSITORY/content" \
  "$REPOSITORY/.plainfeed/backup/activation-interrupted/content"
mv "$REPOSITORY/config" \
  "$REPOSITORY/.plainfeed/backup/activation-interrupted/config"
mkdir -p "$REPOSITORY/content"
RECOVERY_SOURCE=$(find \
  "$REPOSITORY/.plainfeed/backup/activation-interrupted/content" \
  -type f -name '*.md' | head -n 1)
cp "$RECOVERY_SOURCE" "$REPOSITORY/content/partial.md"
LOCAL_RECOVERY=$(wasmtime run \
  --dir "$REPOSITORY::/data" \
  "$WASM" recover-local)
case "$LOCAL_RECOVERY" in
  *"local_recovery=completed"*) ;;
  *) echo "interrupted activation recovery did not complete" >&2; exit 1 ;;
esac
test ! -d "$REPOSITORY/.plainfeed/update.lock"
git -C "$REPOSITORY" diff --quiet
git -C "$REPOSITORY" diff --cached --quiet

git -C "$REPOSITORY" fsck --full >/dev/null

echo "Plainfeed scheduling, conflicts, and local recovery passed under Wasmtime"

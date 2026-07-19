#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 PAT_FILE" >&2
  exit 2
fi

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
PAT_FILE=$1
: "${PLAINFEED_TEST_REMOTE_URL:?set PLAINFEED_TEST_REMOTE_URL to the HTTPS test repository}"
: "${PLAINFEED_TEST_SSH_REMOTE_URL:?set PLAINFEED_TEST_SSH_REMOTE_URL to the SSH test repository}"
REMOTE=$PLAINFEED_TEST_REMOTE_URL
SSH_REMOTE=$PLAINFEED_TEST_SSH_REMOTE_URL
PORT=${PLAINFEED_PUSH_TEST_PORT:-18083}
TEMPORARY=$(mktemp -d "${TMPDIR:-/tmp}/plainfeed-state-push.XXXXXX")
REPOSITORY="$TEMPORARY/repository"
FRESH="$TEMPORARY/fresh"
SERVER_LOG="$TEMPORARY/reader.log"
SYNC_LOG="$TEMPORARY/sync.log"
SERVER_PID=

cleanup() {
  unset PLAINFEED_GITHUB_TOKEN PLAINFEED_REMOTE_URL
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$TEMPORARY"
}
trap cleanup EXIT INT TERM

git clone --quiet "$SSH_REMOTE" "$REPOSITORY"
BASE=$(git -C "$REPOSITORY" rev-parse refs/heads/main)

cargo build --manifest-path "$ROOT/Cargo.toml" \
  -p plainfeed-server --target wasm32-wasip2
RUSTFLAGS=--cfg=tokio_unstable cargo build --manifest-path "$ROOT/Cargo.toml" \
  -p plainfeed-sync --bin plainfeed-sync --target wasm32-wasip2

wasmtime serve \
  -S cli=y \
  --addr "127.0.0.1:$PORT" \
  --dir "$REPOSITORY::/data" \
  "$ROOT/target/wasm32-wasip2/debug/plainfeed_server.wasm" \
  >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!

attempt=0
until curl --fail --silent "http://127.0.0.1:$PORT/health" >/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 50 ]; then
    sed -n '1,160p' "$SERVER_LOG" >&2
    exit 1
  fi
  sleep 0.1
done

ENTRY=20260717-wasip2-reader
STATE="$REPOSITORY/state/entries/$ENTRY.toml"
if grep -q '^favorite = true$' "$STATE"; then
  FAVORITE=false
else
  FAVORITE=true
fi
curl --fail --silent --request POST \
  "http://127.0.0.1:$PORT/entries/$ENTRY/read" >/dev/null
curl --fail --silent --request POST --data "favorite=$FAVORITE" \
  "http://127.0.0.1:$PORT/entries/$ENTRY/favorite" >/dev/null
curl --fail --silent --request POST \
  --data "comment=Plainfeed%20Wasmtime%20state%20publication%20verification" \
  "http://127.0.0.1:$PORT/entries/$ENTRY/comments" >/dev/null
kill "$SERVER_PID"
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=

DIRTY_COUNT=$(find "$REPOSITORY/.plainfeed/dirty" -type f | wc -l | tr -d ' ')
if [ "$DIRTY_COUNT" -lt 2 ]; then
  echo "expected dirty markers for favorite and comment mutations, found $DIRTY_COUNT" >&2
  sed -n '1,160p' "$SERVER_LOG" >&2
  exit 1
fi
PLAINFEED_REMOTE_URL=https://127.0.0.1:1/offline.git
export PLAINFEED_REMOTE_URL
if wasmtime run \
  --env PLAINFEED_REMOTE_URL \
  -S inherit-network=y \
  -S allow-ip-name-lookup=y \
  --dir "$REPOSITORY::/data" \
  "$ROOT/target/wasm32-wasip2/debug/plainfeed-sync.wasm" force \
  >"$SYNC_LOG" 2>&1; then
  echo "offline state publication unexpectedly succeeded" >&2
  exit 1
fi
test "$(find "$REPOSITORY/.plainfeed/dirty" -type f | wc -l | tr -d ' ')" -eq \
  "$DIRTY_COUNT"
PLAINFEED_REMOTE_URL=$REMOTE
PLAINFEED_GITHUB_TOKEN=$(tr -d '\r\n' <"$PAT_FILE")
export PLAINFEED_GITHUB_TOKEN PLAINFEED_REMOTE_URL

if ! wasmtime run \
  --env PLAINFEED_GITHUB_TOKEN \
  --env PLAINFEED_REMOTE_URL \
  -S inherit-network=y \
  -S allow-ip-name-lookup=y \
  --dir "$REPOSITORY::/data" \
  "$ROOT/target/wasm32-wasip2/debug/plainfeed-sync.wasm" force \
  >"$SYNC_LOG" 2>&1; then
  echo "state publication command failed" >&2
  if grep -Fq "$PLAINFEED_GITHUB_TOKEN" "$SYNC_LOG"; then
    echo "diagnostic output withheld because it contains credentials" >&2
  else
    sed -n '1,200p' "$SYNC_LOG" >&2
  fi
  exit 1
fi

grep -q '^pull=completed$' "$SYNC_LOG"
grep -q '^push=completed$' "$SYNC_LOG"
test "$(find "$REPOSITORY/.plainfeed/dirty" -type f | wc -l | tr -d ' ')" -eq 0
if grep -Fq "$PLAINFEED_GITHUB_TOKEN" "$SYNC_LOG"; then
  echo "state push output leaked credentials" >&2
  exit 1
fi

HEAD=$(git -C "$REPOSITORY" rev-parse refs/heads/main)
test "$(git -C "$REPOSITORY" rev-parse "$HEAD^")" = "$BASE"
test "$(git -C "$REPOSITORY" rev-list --count "$BASE..$HEAD")" -eq 1
CHANGED_PATHS=$(git -C "$REPOSITORY" diff-tree \
  --no-commit-id --name-only -r "$BASE" "$HEAD")
test -n "$CHANGED_PATHS"
if printf '%s\n' "$CHANGED_PATHS" | grep -Ev '^state/' >/dev/null; then
  echo "state publication changed a path outside state/" >&2
  printf '%s\n' "$CHANGED_PATHS" >&2
  exit 1
fi
test -f "$REPOSITORY/content/2026/07/20260717-scheduled-sync.md"
if git -C "$REPOSITORY" config --get-regexp 'token|password|authorization' >/dev/null 2>&1; then
  echo "Git configuration contains a credential-like key" >&2
  exit 1
fi
if git -C "$REPOSITORY" log --all --format='%B' | grep -Fq "$PLAINFEED_GITHUB_TOKEN"; then
  echo "Git history leaked credentials" >&2
  exit 1
fi

git clone --quiet "$SSH_REMOTE" "$FRESH"
test "$(git -C "$REPOSITORY" rev-parse refs/heads/main)" = \
  "$(git -C "$FRESH" rev-parse refs/heads/main)"
STATE="$FRESH/state/entries/$ENTRY.toml"
grep -q "^favorite = $FAVORITE$" "$STATE"
grep -q '^read_at = ' "$STATE"
grep -q 'body = "Plainfeed Wasmtime state publication verification"' "$STATE"
git -C "$REPOSITORY" fsck --full >/dev/null
git -C "$FRESH" fsck --full >/dev/null

echo "Plainfeed state publication passed against GitHub under Wasmtime"

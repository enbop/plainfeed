#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SOURCE=${1:-$ROOT/refs/plainfeed-playground}
READER_PORT=${PLAINFEED_RACE_READER_PORT:-18084}
GIT_PORT=${PLAINFEED_RACE_GIT_PORT:-18085}
ADVANCES=${PLAINFEED_RACE_ADVANCES:-1}
TEMPORARY=$(mktemp -d "${TMPDIR:-/tmp}/plainfeed-state-race.XXXXXX")
REMOTE="$TEMPORARY/remote.git"
REPOSITORY="$TEMPORARY/repository"
READER_LOG="$TEMPORARY/reader.log"
GIT_LOG="$TEMPORARY/git.log"
SYNC_LOG="$TEMPORARY/sync.log"
READER_PID=
GIT_PID=

cleanup() {
  for pid in "$READER_PID" "$GIT_PID"; do
    if [ -n "$pid" ]; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  rm -rf "$TEMPORARY"
}
trap cleanup EXIT INT TERM

git clone --quiet --bare "$SOURCE" "$REMOTE"
git clone --quiet "$REMOTE" "$REPOSITORY"
BASE=$(git -C "$REPOSITORY" rev-parse refs/heads/main)

cargo build --manifest-path "$ROOT/Cargo.toml" \
  -p plainfeed-server --target wasm32-wasip2
RUSTFLAGS=--cfg=tokio_unstable cargo build --manifest-path "$ROOT/Cargo.toml" \
  -p plainfeed-sync --bin plainfeed-sync --target wasm32-wasip2

wasmtime serve \
  -S cli=y \
  --addr "127.0.0.1:$READER_PORT" \
  --dir "$REPOSITORY::/data" \
  "$ROOT/target/wasm32-wasip2/debug/plainfeed_server.wasm" \
  >"$READER_LOG" 2>&1 &
READER_PID=$!
attempt=0
until curl --fail --silent "http://127.0.0.1:$READER_PORT/health" >/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 50 ]; then
    sed -n '1,160p' "$READER_LOG" >&2
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
curl --fail --silent --request POST --data "favorite=$FAVORITE" \
  "http://127.0.0.1:$READER_PORT/entries/$ENTRY/favorite" >/dev/null
kill "$READER_PID"
wait "$READER_PID" 2>/dev/null || true
READER_PID=
DIRTY_COUNT=$(find "$REPOSITORY/.plainfeed/dirty" -type f | wc -l | tr -d ' ')
test "$DIRTY_COUNT" -ge 1

python3 "$ROOT/experiments/git-wasi/support/git-smart-http-server.py" \
  "$REMOTE" --port "$GIT_PORT" --advance-pushes "$ADVANCES" \
  >"$GIT_LOG" 2>&1 &
GIT_PID=$!
attempt=0
until curl --fail --silent \
  "http://127.0.0.1:$GIT_PORT/repo.git/info/refs?service=git-upload-pack" \
  >/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 50 ]; then
    sed -n '1,200p' "$GIT_LOG" >&2
    exit 1
  fi
  sleep 0.1
done

PLAINFEED_REMOTE_URL="http://127.0.0.1:$GIT_PORT/repo.git"
export PLAINFEED_REMOTE_URL
run_sync() {
  wasmtime run \
  --env PLAINFEED_REMOTE_URL \
  -S inherit-network=y \
  --dir "$REPOSITORY::/data" \
  "$ROOT/target/wasm32-wasip2/debug/plainfeed-sync.wasm" "$1"
}

if [ "$ADVANCES" -ge 3 ]; then
  if run_sync force >"$SYNC_LOG" 2>&1; then
    echo "state publication unexpectedly survived three remote advances" >&2
    exit 1
  fi
  test "$(find "$REPOSITORY/.plainfeed/dirty" -type f | wc -l | tr -d ' ')" -eq \
    "$DIRTY_COUNT"
  test -f "$REPOSITORY/.plainfeed/conflict.toml"
  REMOTE_HEAD=$(git --git-dir "$REMOTE" rev-parse refs/heads/main)
  grep -q 'lost the remote race three times' "$REPOSITORY/.plainfeed/conflict.toml"
  grep -q "remote_tip = \"$REMOTE_HEAD\"" "$REPOSITORY/.plainfeed/conflict.toml"
  STATUS=$(wasmtime run \
    --dir "$REPOSITORY::/data" \
    "$ROOT/target/wasm32-wasip2/debug/plainfeed-sync.wasm" status)
  case "$STATUS" in
    *"conflict_active=true"*"lost the remote race three times"*) ;;
    *) echo "retry exhaustion was not exposed by status" >&2; exit 1 ;;
  esac
  wasmtime run \
    --dir "$REPOSITORY::/data" \
    "$ROOT/target/wasm32-wasip2/debug/plainfeed-sync.wasm" \
    acknowledge-conflict >/dev/null
  if ! run_sync force >"$SYNC_LOG" 2>&1; then
    sed -n '1,200p' "$SYNC_LOG" >&2
    exit 1
  fi
  grep -q '^push=completed$' "$SYNC_LOG"
  test ! -f "$REPOSITORY/.plainfeed/conflict.toml"
  test "$(find "$REPOSITORY/.plainfeed/dirty" -type f | wc -l | tr -d ' ')" -eq 0
  test -f "$REPOSITORY/content/2026/07/race-content-3.md"
  git --git-dir "$REMOTE" fsck --full >/dev/null
  git -C "$REPOSITORY" fsck --full >/dev/null
  echo "Plainfeed reported and recovered from exhausted state-publication races"
  exit 0
fi

if ! run_sync force >"$SYNC_LOG" 2>&1; then
  sed -n '1,200p' "$SYNC_LOG" >&2
  sed -n '1,200p' "$GIT_LOG" >&2
  exit 1
fi

grep -q '^push=completed$' "$SYNC_LOG"
HEAD=$(git --git-dir "$REMOTE" rev-parse refs/heads/main)
test "$(git --git-dir "$REMOTE" rev-list --count "$BASE..$HEAD")" -eq 2
test "$(git --git-dir "$REMOTE" log -1 --format=%s "$HEAD^")" = \
  "test: race state publication"
CHANGED_PATHS=$(git --git-dir "$REMOTE" diff-tree \
  --no-commit-id --name-only -r "$HEAD^" "$HEAD")
test -n "$CHANGED_PATHS"
if printf '%s\n' "$CHANGED_PATHS" | grep -Ev '^state/' >/dev/null; then
  echo "rebuilt state candidate changed a path outside state/" >&2
  exit 1
fi
test -f "$REPOSITORY/content/2026/07/race-content.md"
REMOTE_STATE=$(git --git-dir "$REMOTE" show "$HEAD:state/entries/$ENTRY.toml")
printf '%s\n' "$REMOTE_STATE" | grep -q "^favorite = $FAVORITE$"
test "$(find "$REPOSITORY/.plainfeed/dirty" -type f | wc -l | tr -d ' ')" -eq 0
git --git-dir "$REMOTE" fsck --full >/dev/null
git -C "$REPOSITORY" fsck --full >/dev/null

echo "Plainfeed preserved a racing content commit during Wasmtime state publication"

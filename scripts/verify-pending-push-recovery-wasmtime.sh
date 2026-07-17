#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SOURCE=${1:-$ROOT/refs/plainfeed-playground}
TEMPORARY=$(mktemp -d "${TMPDIR:-/tmp}/plainfeed-pending-push.XXXXXX")
REMOTE="$TEMPORARY/remote.git"
REPOSITORY="$TEMPORARY/repository"
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

RUSTFLAGS=--cfg=tokio_unstable cargo build --manifest-path "$ROOT/Cargo.toml" \
  -p plainfeed-sync --bin plainfeed-sync --target wasm32-wasip2
cargo build --manifest-path "$ROOT/Cargo.toml" \
  -p plainfeed-server --target wasm32-wasip2

READER_PORT=18189
wasmtime serve \
  -S cli=y \
  --addr "127.0.0.1:$READER_PORT" \
  --dir "$REPOSITORY::/data" \
  "$ROOT/target/wasm32-wasip2/debug/plainfeed_server.wasm" \
  >"$TEMPORARY/reader.log" 2>&1 &
READER_PID=$!
attempt=0
until curl --fail --silent "http://127.0.0.1:$READER_PORT/health" >/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 50 ]; then
    sed -n '1,160p' "$TEMPORARY/reader.log" >&2
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
test "$DIRTY_COUNT" -gt 0

GIT_PORT=18190
python3 "$ROOT/experiments/git-wasi/support/git-smart-http-server.py" \
  "$REMOTE" --port "$GIT_PORT" --drop-first-push-response \
  >"$TEMPORARY/git.log" 2>&1 &
GIT_PID=$!
attempt=0
until curl --fail --silent \
  "http://127.0.0.1:$GIT_PORT/repo.git/info/refs?service=git-upload-pack" \
  >/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 50 ]; then
    sed -n '1,200p' "$TEMPORARY/git.log" >&2
    exit 1
  fi
  sleep 0.1
done

PLAINFEED_REMOTE_URL="http://127.0.0.1:$GIT_PORT/repo.git"
export PLAINFEED_REMOTE_URL
run_force() {
  wasmtime run \
    --env PLAINFEED_REMOTE_URL \
    -S inherit-network=y \
    --dir "$REPOSITORY::/data" \
    "$ROOT/target/wasm32-wasip2/debug/plainfeed-sync.wasm" force
}

if run_force >"$TEMPORARY/first.log" 2>&1; then
  echo "push with a dropped response unexpectedly reported success" >&2
  exit 1
fi
REMOTE_AFTER_PUSH=$(git --git-dir "$REMOTE" rev-parse refs/heads/main)
test "$REMOTE_AFTER_PUSH" != "$BASE"
test "$(git -C "$REPOSITORY" rev-parse refs/heads/main)" = "$BASE"
test -f "$REPOSITORY/.plainfeed/pending-push.toml"
test "$(find "$REPOSITORY/.plainfeed/dirty" -type f | wc -l | tr -d ' ')" -eq \
  "$DIRTY_COUNT"

SECOND=$(run_force)
case "$SECOND" in
  *"pull=completed"*"push=already-published"*) ;;
  *) echo "next force did not recover the confirmed pending push" >&2; exit 1 ;;
esac
test "$(git -C "$REPOSITORY" rev-parse refs/heads/main)" = "$REMOTE_AFTER_PUSH"
test ! -f "$REPOSITORY/.plainfeed/pending-push.toml"
test "$(find "$REPOSITORY/.plainfeed/dirty" -type f | wc -l | tr -d ' ')" -eq 0
grep -q "^favorite = $FAVORITE$" "$STATE"
git --git-dir "$REMOTE" fsck --full >/dev/null
git -C "$REPOSITORY" fsck --full >/dev/null

echo "Plainfeed recovered an accepted push after its HTTP response was lost"

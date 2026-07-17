#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SOURCE=${1:-$ROOT/refs/plainfeed-playground}
TEMPORARY=$(mktemp -d "${TMPDIR:-/tmp}/plainfeed-concurrent-state.XXXXXX")
REMOTE="$TEMPORARY/remote.git"
REPOSITORY="$TEMPORARY/repository"
READER_PID=
GIT_PID=
SYNC_PID=

cleanup() {
  for pid in "$SYNC_PID" "$READER_PID" "$GIT_PID"; do
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
RUSTFLAGS=--cfg=tokio_unstable cargo build --manifest-path "$ROOT/Cargo.toml" \
  -p plainfeed-sync --bin plainfeed-sync --target wasm32-wasip2
cargo build --manifest-path "$ROOT/Cargo.toml" \
  -p plainfeed-server --target wasm32-wasip2

READER_PORT=18191
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

GIT_PORT=18192
python3 "$ROOT/experiments/git-wasi/support/git-smart-http-server.py" \
  "$REMOTE" --port "$GIT_PORT" --delay-first-push-response-ms 3000 \
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

run_force >"$TEMPORARY/first-sync.log" 2>&1 &
SYNC_PID=$!
attempt=0
until grep -q 'delaying receive-pack response' "$TEMPORARY/git.log"; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 100 ]; then
    sed -n '1,200p' "$TEMPORARY/git.log" >&2
    sed -n '1,200p' "$TEMPORARY/first-sync.log" >&2
    exit 1
  fi
  sleep 0.1
done

COMMENT='Concurrent mutation survives captured marker cleanup'
curl --fail --silent --request POST \
  --data "comment=Concurrent%20mutation%20survives%20captured%20marker%20cleanup" \
  "http://127.0.0.1:$READER_PORT/entries/$ENTRY/comments" >/dev/null
wait "$SYNC_PID"
SYNC_PID=
grep -q '^push=completed$' "$TEMPORARY/first-sync.log"
test "$(find "$REPOSITORY/.plainfeed/dirty" -type f | wc -l | tr -d ' ')" -eq 1
grep -q "body = \"$COMMENT\"" "$STATE"
FIRST_REMOTE=$(git --git-dir "$REMOTE" rev-parse refs/heads/main)
if git --git-dir "$REMOTE" show "$FIRST_REMOTE:state/entries/$ENTRY.toml" | \
  grep -Fq "$COMMENT"; then
  echo "the first candidate unexpectedly included the concurrent mutation" >&2
  exit 1
fi

SECOND=$(run_force)
case "$SECOND" in
  *"push=completed"*) ;;
  *) echo "the concurrent mutation was not published by the next force" >&2; exit 1 ;;
esac
SECOND_REMOTE=$(git --git-dir "$REMOTE" rev-parse refs/heads/main)
git --git-dir "$REMOTE" show "$SECOND_REMOTE:state/entries/$ENTRY.toml" | \
  grep -Fq "$COMMENT"
test "$(find "$REPOSITORY/.plainfeed/dirty" -type f | wc -l | tr -d ' ')" -eq 0
git --git-dir "$REMOTE" fsck --full >/dev/null
git -C "$REPOSITORY" fsck --full >/dev/null

echo "Plainfeed preserved and later published a state mutation concurrent with push"

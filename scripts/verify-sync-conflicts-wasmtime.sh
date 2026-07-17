#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SOURCE=${1:-$ROOT/refs/plainfeed-playground}
TEMPORARY=$(mktemp -d "${TMPDIR:-/tmp}/plainfeed-conflicts.XXXXXX")
WASM="$ROOT/target/wasm32-wasip2/debug/plainfeed-sync.wasm"
READER_WASM="$ROOT/target/wasm32-wasip2/debug/plainfeed_server.wasm"
SERVER_PID=

cleanup() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$TEMPORARY"
}
trap cleanup EXIT INT TERM

RUSTFLAGS=--cfg=tokio_unstable cargo build --manifest-path "$ROOT/Cargo.toml" \
  -p plainfeed-sync --bin plainfeed-sync --target wasm32-wasip2
cargo build --manifest-path "$ROOT/Cargo.toml" \
  -p plainfeed-server --target wasm32-wasip2

start_git_server() {
  repository=$1
  port=$2
  shift 2
  python3 "$ROOT/experiments/git-wasi/support/git-smart-http-server.py" \
    "$repository" --port "$port" "$@" \
    >"$TEMPORARY/git-$port.log" 2>&1 &
  SERVER_PID=$!
  attempt=0
  until curl --fail --silent \
    "http://127.0.0.1:$port/repo.git/info/refs?service=git-upload-pack" \
    >/dev/null; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 50 ]; then
      sed -n '1,200p' "$TEMPORARY/git-$port.log" >&2
      exit 1
    fi
    sleep 0.1
  done
}

stop_server() {
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=
}

run_force() {
  repository=$1
  port=$2
  PLAINFEED_REMOTE_URL="http://127.0.0.1:$port/repo.git"
  export PLAINFEED_REMOTE_URL
  wasmtime run \
    --env PLAINFEED_REMOTE_URL \
    -S inherit-network=y \
    --dir "$repository::/data" \
    "$WASM" force
}

# A rewritten remote main is never merged into the live checkout.
DIVERGED_REMOTE="$TEMPORARY/diverged.git"
DIVERGED_LIVE="$TEMPORARY/diverged-live"
git clone --quiet --bare "$SOURCE" "$DIVERGED_REMOTE"
git clone --quiet "$DIVERGED_REMOTE" "$DIVERGED_LIVE"
DIVERGED_BASE=$(git -C "$DIVERGED_LIVE" rev-parse refs/heads/main)
DIVERGED_TREE=$(git --git-dir "$DIVERGED_REMOTE" rev-parse 'refs/heads/main^{tree}')
DIVERGED_COMMIT=$(printf '%s\n' 'test: unrelated remote history' | \
  GIT_AUTHOR_NAME='Plainfeed divergence fixture' \
  GIT_AUTHOR_EMAIL='divergence@plainfeed.invalid' \
  GIT_COMMITTER_NAME='Plainfeed divergence fixture' \
  GIT_COMMITTER_EMAIL='divergence@plainfeed.invalid' \
  git --git-dir "$DIVERGED_REMOTE" commit-tree "$DIVERGED_TREE")
git --git-dir "$DIVERGED_REMOTE" update-ref refs/heads/main "$DIVERGED_COMMIT"
start_git_server "$DIVERGED_REMOTE" 18186
if run_force "$DIVERGED_LIVE" 18186 >"$TEMPORARY/diverged-sync.log" 2>&1; then
  echo "divergent remote history unexpectedly synchronized" >&2
  exit 1
fi
test "$(git -C "$DIVERGED_LIVE" rev-parse refs/heads/main)" = "$DIVERGED_BASE"
grep -q 'not a descendant' "$DIVERGED_LIVE/.plainfeed/conflict.toml"
stop_server

# Three consecutive remote advances exhaust CAS retries and retain dirty state.
RACE_REMOTE="$TEMPORARY/race.git"
RACE_LIVE="$TEMPORARY/race-live"
git clone --quiet --bare "$SOURCE" "$RACE_REMOTE"
git clone --quiet "$RACE_REMOTE" "$RACE_LIVE"
RACE_BASE=$(git -C "$RACE_LIVE" rev-parse refs/heads/main)
READER_PORT=18187
wasmtime serve \
  -S cli=y \
  --addr "127.0.0.1:$READER_PORT" \
  --dir "$RACE_LIVE::/data" \
  "$READER_WASM" >"$TEMPORARY/reader.log" 2>&1 &
SERVER_PID=$!
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
STATE="$RACE_LIVE/state/entries/$ENTRY.toml"
if grep -q '^favorite = true$' "$STATE"; then
  FAVORITE=false
else
  FAVORITE=true
fi
curl --fail --silent --request POST --data "favorite=$FAVORITE" \
  "http://127.0.0.1:$READER_PORT/entries/$ENTRY/favorite" >/dev/null
stop_server
DIRTY_BEFORE=$(find "$RACE_LIVE/.plainfeed/dirty" -type f | wc -l | tr -d ' ')
test "$DIRTY_BEFORE" -gt 0

start_git_server "$RACE_REMOTE" 18188 --advance-pushes 3
if run_force "$RACE_LIVE" 18188 >"$TEMPORARY/race-sync.log" 2>&1; then
  echo "three consecutive publication races unexpectedly succeeded" >&2
  exit 1
fi
grep -q 'lost the remote race three times' "$RACE_LIVE/.plainfeed/conflict.toml"
test "$(find "$RACE_LIVE/.plainfeed/dirty" -type f | wc -l | tr -d ' ')" -eq \
  "$DIRTY_BEFORE"
RACE_HEAD=$(git --git-dir "$RACE_REMOTE" rev-parse refs/heads/main)
test "$(git --git-dir "$RACE_REMOTE" rev-list --count "$RACE_BASE..$RACE_HEAD")" -eq 3
git --git-dir "$RACE_REMOTE" fsck --full >/dev/null
git -C "$RACE_LIVE" fsck --full >/dev/null
stop_server

echo "Plainfeed divergence and retry-exhaustion conflicts passed under Wasmtime"

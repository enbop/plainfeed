#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SOURCE=${1:-$ROOT/refs/plainfeed-playground}
BASE_PORT=${PLAINFEED_FAILURE_GIT_PORT:-18193}
TEMPORARY=$(mktemp -d "${TMPDIR:-/tmp}/plainfeed-network-failures.XXXXXX")
WASM="$ROOT/target/wasm32-wasip2/debug/plainfeed-sync.wasm"
SERVER_PID=
CASE_NUMBER=0

cleanup() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$TEMPORARY"
}
trap cleanup EXIT INT TERM

run_case() {
  name=$1
  shift
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    SERVER_PID=
  fi
  CASE_NUMBER=$((CASE_NUMBER + 1))
  CASE_ROOT="$TEMPORARY/$name"
  REMOTE="$CASE_ROOT/remote.git"
  REPOSITORY="$CASE_ROOT/repository"
  PRODUCER="$CASE_ROOT/producer"
  LOG="$CASE_ROOT/sync.log"
  SERVER_LOG="$CASE_ROOT/server.log"
  mkdir -p "$CASE_ROOT"
  git clone --quiet --bare "$SOURCE" "$REMOTE"
  git clone --quiet "$REMOTE" "$REPOSITORY"
  git clone --quiet "$REMOTE" "$PRODUCER"
  git -C "$PRODUCER" config user.name "Plainfeed network fixture"
  git -C "$PRODUCER" config user.email "network@plainfeed.invalid"
  BASE=$(git -C "$REPOSITORY" rev-parse refs/heads/main)
  ENTRY=content/2026/07/20260717-wasip2-reader.md
  printf '\nNetwork recovery fixture: %s.\n' "$name" >>"$PRODUCER/$ENTRY"
  git -C "$PRODUCER" add "$ENTRY"
  git -C "$PRODUCER" commit --quiet -m "test: prepare $name network failure"
  git -C "$PRODUCER" push --quiet origin main

  PORT=$((BASE_PORT + CASE_NUMBER - 1))
  python3 "$ROOT/experiments/git-wasi/support/git-smart-http-server.py" \
    "$REMOTE" --port "$PORT" "$@" >"$SERVER_LOG" 2>&1 &
  SERVER_PID=$!
  attempt=0
  until curl --fail --silent \
    "http://127.0.0.1:$PORT/repo.git/info/refs?service=git-upload-pack" \
    >/dev/null; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 50 ]; then
      sed -n '1,160p' "$SERVER_LOG" >&2
      exit 1
    fi
    sleep 0.1
  done

  PLAINFEED_REMOTE_URL="http://127.0.0.1:$PORT/repo.git"
  export PLAINFEED_REMOTE_URL
  run_force() {
    wasmtime run \
      --env PLAINFEED_REMOTE_URL \
      -S inherit-network=y \
      --dir "$REPOSITORY::/data" \
      "$WASM" force
  }

  if run_force >"$LOG" 2>&1; then
    echo "$name failure unexpectedly synchronized" >&2
    exit 1
  fi
  test "$(git -C "$REPOSITORY" rev-parse refs/heads/main)" = "$BASE"
  ! grep -q "Network recovery fixture: $name" "$REPOSITORY/$ENTRY"
  test ! -f "$REPOSITORY/.plainfeed/conflict.toml"
  test -f "$REPOSITORY/.plainfeed/sync.toml"
  ERROR_LENGTH=$(sed -n 's/^last_error = "\(.*\)"$/\1/p' \
    "$REPOSITORY/.plainfeed/sync.toml" | wc -c | tr -d ' ')
  test "$ERROR_LENGTH" -le 4097

  run_force >"$LOG" 2>&1
  grep -q '^pull=completed$' "$LOG"
  grep -q "Network recovery fixture: $name" "$REPOSITORY/$ENTRY"
  test "$(git -C "$REPOSITORY" rev-parse refs/heads/main)" = \
    "$(git --git-dir "$REMOTE" rev-parse refs/heads/main)"
  git -C "$REPOSITORY" fsck --full >/dev/null
  git --git-dir "$REMOTE" fsck --full >/dev/null
}

run_case rate-limit --fail-first-fetch-status 429
run_case corrupt-pack --corrupt-first-fetch-response
run_case timeout --delay-first-fetch-response-ms 31000

echo "Plainfeed rate-limit, corrupt-pack, and timeout recovery passed under Wasmtime"

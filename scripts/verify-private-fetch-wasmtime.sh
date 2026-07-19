#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: $0 PAT_FILE EXPECTED_CHECKOUT" >&2
  exit 2
fi

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
PAT_FILE=$1
EXPECTED_CHECKOUT=$2
: "${PLAINFEED_TEST_REMOTE_URL:?set PLAINFEED_TEST_REMOTE_URL to the HTTPS test repository}"
REMOTE=$PLAINFEED_TEST_REMOTE_URL
WASM="$ROOT/target/wasm32-wasip2/debug/plainfeed-fetch.wasm"
DATA=$(mktemp -d "/tmp/plainfeed-private-fetch.XXXXXX")
LOG="$DATA/fetch.log"

cleanup() {
  unset PLAINFEED_GITHUB_TOKEN
  rm -rf "$DATA"
}
trap cleanup EXIT INT TERM

if [ ! -f "$PAT_FILE" ]; then
  echo "PAT file does not exist: $PAT_FILE" >&2
  exit 1
fi

PLAINFEED_GITHUB_TOKEN=$(tr -d '\r\n' <"$PAT_FILE")
if [ -z "$PLAINFEED_GITHUB_TOKEN" ]; then
  echo "PAT file is empty" >&2
  exit 1
fi
export PLAINFEED_GITHUB_TOKEN

run_fetch() {
  if wasmtime run \
    --env PLAINFEED_GITHUB_TOKEN \
    -S inherit-network=y \
    -S allow-ip-name-lookup=y \
    --dir /tmp \
    "$WASM" \
    "$REMOTE" "$DATA/repository" >>"$LOG" 2>&1; then
    return 0
  fi
  if grep -Fq "$PLAINFEED_GITHUB_TOKEN" "$LOG"; then
    echo "fetch failed and its output contained the credential; output suppressed" >&2
  else
    sed -n '1,160p' "$LOG" >&2
  fi
  return 1
}

run_fetch
FIRST_TIP=$(git -C "$DATA/repository" rev-parse refs/remotes/origin/main)
run_fetch
SECOND_TIP=$(git -C "$DATA/repository" rev-parse refs/remotes/origin/main)
EXPECTED_TIP=$(git -C "$EXPECTED_CHECKOUT" rev-parse origin/main)

if [ "$FIRST_TIP" != "$EXPECTED_TIP" ] || [ "$SECOND_TIP" != "$EXPECTED_TIP" ]; then
  echo "WASI fetch tip does not match the expected remote tip" >&2
  exit 1
fi
if [ -e "$DATA/repository/content" ] || [ -e "$DATA/repository/state" ]; then
  echo "pull-only fetch unexpectedly changed live worktree files" >&2
  exit 1
fi
if grep -Fq "$PLAINFEED_GITHUB_TOKEN" "$LOG"; then
  echo "credential appeared in fetch output" >&2
  exit 1
fi
if git -C "$DATA/repository" config --list | grep -Fq "$PLAINFEED_GITHUB_TOKEN"; then
  echo "credential appeared in Git configuration" >&2
  exit 1
fi

git -C "$DATA/repository" fsck --full >/dev/null
grep -q "^remote_tip=$EXPECTED_TIP$" "$LOG"
grep -q '^state_tree=[0-9a-f]\{40\}$' "$LOG"

PLAINFEED_MAX_RESPONSE_BYTES=32
export PLAINFEED_MAX_RESPONSE_BYTES
if wasmtime run \
  --env PLAINFEED_GITHUB_TOKEN \
  --env PLAINFEED_MAX_RESPONSE_BYTES \
  -S inherit-network=y \
  -S allow-ip-name-lookup=y \
  --dir /tmp \
  "$WASM" \
  "$REMOTE" "$DATA/limited-repository" >>"$LOG" 2>&1; then
  echo "fetch unexpectedly succeeded above its response limit" >&2
  exit 1
fi
unset PLAINFEED_MAX_RESPONSE_BYTES
if [ -e "$DATA/limited-repository/content" ] || [ -e "$DATA/limited-repository/state" ]; then
  echo "failed fetch unexpectedly changed live worktree files" >&2
  exit 1
fi
if ! grep -q 'over the 32-byte limit' "$LOG"; then
  if grep -Fq "$PLAINFEED_GITHUB_TOKEN" "$LOG"; then
    echo "limit verification failed and output contained the credential; output suppressed" >&2
  else
    sed -n '1,200p' "$LOG" >&2
  fi
  exit 1
fi

echo "Plainfeed private HTTPS fetch passed twice under Wasmtime"

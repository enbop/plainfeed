#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SOURCE=${1:-$ROOT/refs/plainfeed-data-fixture}
PORT=${PLAINFEED_CONFLICT_GIT_PORT:-18086}
TEMPORARY=$(mktemp -d "${TMPDIR:-/tmp}/plainfeed-conflicts.XXXXXX")
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

setup_case() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    SERVER_PID=
  fi
  CASE_NUMBER=$((CASE_NUMBER + 1))
  CASE_ROOT="$TEMPORARY/case-$CASE_NUMBER"
  REMOTE="$CASE_ROOT/remote.git"
  REPOSITORY="$CASE_ROOT/repository"
  PRODUCER="$CASE_ROOT/producer"
  LOG="$CASE_ROOT/sync.log"
  SERVER_LOG="$CASE_ROOT/server.log"
  mkdir -p "$CASE_ROOT"
  git clone --quiet --bare "$SOURCE" "$REMOTE"
  git clone --quiet "$REMOTE" "$REPOSITORY"
  git clone --quiet "$REMOTE" "$PRODUCER"
  git -C "$PRODUCER" config user.name "Plainfeed conflict fixture"
  git -C "$PRODUCER" config user.email "conflict@plainfeed.invalid"
  BASE=$(git -C "$REPOSITORY" rev-parse refs/heads/main)
  CASE_PORT=$((PORT + CASE_NUMBER - 1))
  python3 "$ROOT/experiments/git-wasi/support/git-smart-http-server.py" \
    "$REMOTE" --port "$CASE_PORT" >"$SERVER_LOG" 2>&1 &
  SERVER_PID=$!
  attempt=0
  until curl --fail --silent \
    "http://127.0.0.1:$CASE_PORT/repo.git/info/refs?service=git-upload-pack" \
    >/dev/null; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 50 ]; then
      sed -n '1,160p' "$SERVER_LOG" >&2
      exit 1
    fi
    sleep 0.1
  done
  PLAINFEED_REMOTE_URL="http://127.0.0.1:$CASE_PORT/repo.git"
  export PLAINFEED_REMOTE_URL
}

run_sync() {
  wasmtime run \
    --env PLAINFEED_REMOTE_URL \
    -S inherit-network=y \
    --dir "$REPOSITORY::/data" \
    "$WASM" "$1"
}

acknowledge() {
  wasmtime run --dir "$REPOSITORY::/data" "$WASM" \
    acknowledge-conflict >/dev/null
}

expect_conflict() {
  expected=$1
  if run_sync force >"$LOG" 2>&1; then
    echo "sync unexpectedly accepted conflict fixture: $expected" >&2
    exit 1
  fi
  test "$(git -C "$REPOSITORY" rev-parse refs/heads/main)" = "$BASE"
  test -f "$REPOSITORY/.plainfeed/conflict.toml"
  grep -q "$expected" "$REPOSITORY/.plainfeed/conflict.toml"
  status=$(wasmtime run --dir "$REPOSITORY::/data" "$WASM" status)
  case "$status" in
    *"conflict_active=true"*) ;;
    *) echo "status did not expose conflict: $expected" >&2; exit 1 ;;
  esac
}

recover_from_revert() {
  git -C "$PRODUCER" revert --quiet --no-edit HEAD
  git -C "$PRODUCER" push --quiet origin main
  if run_sync force >"$LOG" 2>&1; then
    echo "unacknowledged conflict unexpectedly synchronized" >&2
    exit 1
  fi
  acknowledge
  run_sync force >"$LOG" 2>&1
  grep -q '^pull=completed$' "$LOG"
  test ! -f "$REPOSITORY/.plainfeed/conflict.toml"
  test "$(git -C "$REPOSITORY" rev-parse refs/heads/main)" = \
    "$(git --git-dir "$REMOTE" rev-parse refs/heads/main)"
  git -C "$REPOSITORY" fsck --full >/dev/null
  git --git-dir "$REMOTE" fsck --full >/dev/null
}

# A producer touching Plainfeed-owned state is never activated.
setup_case
STATE=state/entries/20260717-wasip2-reader.toml
STATE_BEFORE=$(git -C "$REPOSITORY" hash-object "$REPOSITORY/$STATE")
printf '\nfixture_remote_edit = true\n' >>"$PRODUCER/$STATE"
git -C "$PRODUCER" add "$STATE"
git -C "$PRODUCER" commit --quiet -m "test: modify remote state"
git -C "$PRODUCER" push --quiet origin main
expect_conflict "$STATE"
test "$(git -C "$REPOSITORY" hash-object "$REPOSITORY/$STATE")" = "$STATE_BEFORE"
recover_from_revert

# Removing the complete remote state tree is the same ownership violation.
setup_case
STATE_BEFORE=$(git -C "$REPOSITORY" rev-parse HEAD:state)
git -C "$PRODUCER" rm --quiet -r state
git -C "$PRODUCER" commit --quiet -m "test: remove remote state tree"
git -C "$PRODUCER" push --quiet origin main
expect_conflict 'no state tree'
test "$(git -C "$REPOSITORY" rev-parse HEAD:state)" = "$STATE_BEFORE"
recover_from_revert

# Unknown repository-root paths violate the agreed layout.
setup_case
printf 'unexpected repository path\n' >"$PRODUCER/UNEXPECTED.txt"
git -C "$PRODUCER" add UNEXPECTED.txt
git -C "$PRODUCER" commit --quiet -m "test: add unexpected root path"
git -C "$PRODUCER" push --quiet origin main
expect_conflict 'UNEXPECTED.txt'
test ! -e "$REPOSITORY/UNEXPECTED.txt"
recover_from_revert

# Invalid producer content is staged and rejected before it becomes visible.
setup_case
ENTRY=content/2026/07/20260717-wasip2-reader.md
ENTRY_BEFORE=$(git -C "$REPOSITORY" hash-object "$REPOSITORY/$ENTRY")
printf 'invalid Plainfeed entry\n' >"$PRODUCER/$ENTRY"
git -C "$PRODUCER" add "$ENTRY"
git -C "$PRODUCER" commit --quiet -m "test: publish invalid content"
git -C "$PRODUCER" push --quiet origin main
expect_conflict "$ENTRY"
test "$(git -C "$REPOSITORY" hash-object "$REPOSITORY/$ENTRY")" = "$ENTRY_BEFORE"
recover_from_revert

# Invalid owner-managed configuration is also rejected before activation.
setup_case
CONFIG=config/channels.toml
CONFIG_BEFORE=$(git -C "$REPOSITORY" hash-object "$REPOSITORY/$CONFIG")
printf 'format = "invalid.channels/v9"\n' >"$PRODUCER/$CONFIG"
git -C "$PRODUCER" add "$CONFIG"
git -C "$PRODUCER" commit --quiet -m "test: publish invalid configuration"
git -C "$PRODUCER" push --quiet origin main
expect_conflict "$CONFIG"
test "$(git -C "$REPOSITORY" hash-object "$REPOSITORY/$CONFIG")" = "$CONFIG_BEFORE"
recover_from_revert

# A remote without the agreed canonical main ref is a repository-shape conflict.
setup_case
git --git-dir "$REMOTE" update-ref -d refs/heads/main
if run_sync force >"$LOG" 2>&1; then
  echo "sync unexpectedly accepted a remote without refs/heads/main" >&2
  exit 1
fi
if [ ! -f "$REPOSITORY/.plainfeed/conflict.toml" ]; then
  sed -n '1,200p' "$LOG" >&2
  echo "missing-main failure did not create conflict.toml" >&2
  exit 1
fi
grep -q 'refs/heads/main' "$REPOSITORY/.plainfeed/conflict.toml"
git -C "$PRODUCER" push --quiet origin main
acknowledge
run_sync force >"$LOG" 2>&1
test ! -f "$REPOSITORY/.plainfeed/conflict.toml"

# A force-pushed sibling is observable through origin/main but never activated.
setup_case
ENTRY=content/2026/07/20260717-wasip2-reader.md
printf '\nLinear fixture commit.\n' >>"$PRODUCER/$ENTRY"
git -C "$PRODUCER" add "$ENTRY"
git -C "$PRODUCER" commit --quiet -m "test: establish local linear tip"
git -C "$PRODUCER" push --quiet origin main
run_sync force >"$LOG" 2>&1
LINEAR=$(git -C "$REPOSITORY" rev-parse refs/heads/main)
test "$LINEAR" != "$BASE"
BASE=$LINEAR
git -C "$PRODUCER" reset --quiet --hard "$LINEAR^"
printf '\nDivergent fixture commit.\n' >>"$PRODUCER/$ENTRY"
git -C "$PRODUCER" add "$ENTRY"
git -C "$PRODUCER" commit --quiet -m "test: create divergent remote tip"
git -C "$PRODUCER" push --quiet --force origin main
expect_conflict 'not a descendant'
grep -q 'Linear fixture commit' "$REPOSITORY/$ENTRY"
git -C "$PRODUCER" reset --quiet --hard "$LINEAR"
printf '\nManually resolved after divergence.\n' >>"$PRODUCER/$ENTRY"
git -C "$PRODUCER" add "$ENTRY"
git -C "$PRODUCER" commit --quiet -m "test: resolve divergent history"
git -C "$PRODUCER" push --quiet --force origin main
acknowledge
run_sync force >"$LOG" 2>&1
grep -q 'Manually resolved after divergence' "$REPOSITORY/$ENTRY"
test ! -f "$REPOSITORY/.plainfeed/conflict.toml"
git -C "$REPOSITORY" fsck --full >/dev/null
git --git-dir "$REMOTE" fsck --full >/dev/null

echo "Plainfeed conflict fixtures and manual recovery passed under Wasmtime"

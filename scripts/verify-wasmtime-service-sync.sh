#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SOURCE=${1:-$ROOT/refs/plainfeed-playground}
WASMTIME_BIN=${WASMTIME_BIN:-wasmtime}
HTTP_PORT=${PLAINFEED_SERVICE_HTTP_PORT:-18093}
GIT_PORT=${PLAINFEED_SERVICE_GIT_PORT:-18094}
TEMPORARY=$(mktemp -d "${TMPDIR:-/tmp}/plainfeed-service-sync.XXXXXX")
REMOTE="$TEMPORARY/remote.git"
LIVE="$TEMPORARY/live"
PRODUCER="$TEMPORARY/producer"
GIT_LOG="$TEMPORARY/git.log"
SERVICE_LOG="$TEMPORARY/service.log"
GIT_PID=
SERVICE_PID=

cleanup() {
  for pid in "$SERVICE_PID" "$GIT_PID"; do
    if [ -n "$pid" ]; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  rm -rf "$TEMPORARY"
}
trap cleanup EXIT INT TERM

git clone --quiet --bare "$SOURCE" "$REMOTE"
mkdir "$LIVE"
git clone --quiet "$REMOTE" "$PRODUCER"
mkdir -p "$PRODUCER/content/2026/07"
cp "$ROOT/experiments/wasmtime-run-service/fixtures/service-content.md" \
  "$PRODUCER/content/2026/07/service-daemon-content.md"
git -C "$PRODUCER" add content/2026/07/service-daemon-content.md
git -C "$PRODUCER" \
  -c user.name='Plainfeed service fixture' \
  -c user.email='service-fixture@plainfeed.invalid' \
  commit --quiet -m 'test: publish service content'
git -C "$PRODUCER" push --quiet origin main
CONTENT_HEAD=$(git --git-dir "$REMOTE" rev-parse refs/heads/main)

python3 "$ROOT/experiments/git-wasi/support/git-smart-http-server.py" \
  "$REMOTE" --port "$GIT_PORT" >"$GIT_LOG" 2>&1 &
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

RUSTFLAGS=--cfg=tokio_unstable cargo build --manifest-path "$ROOT/Cargo.toml" \
  -p plainfeed-service --target wasm32-wasip2

PLAINFEED_SYNC_TICK_SECONDS=1
export PLAINFEED_SYNC_TICK_SECONDS
"$WASMTIME_BIN" run \
  --env PLAINFEED_SYNC_TICK_SECONDS \
  -S inherit-network=y \
  --dir "$LIVE::/data" \
  "$ROOT/target/wasm32-wasip2/debug/plainfeed-service.wasm" \
  "127.0.0.1:$HTTP_PORT" /data >"$SERVICE_LOG" 2>&1 &
SERVICE_PID=$!

attempt=0
until curl --fail --silent "http://127.0.0.1:$HTTP_PORT/health" >/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 50 ]; then
    sed -n '1,240p' "$SERVICE_LOG" >&2
    exit 1
  fi
  sleep 0.1
done

test "$(curl --silent --output /dev/null --write-out '%{http_code}' \
  "http://127.0.0.1:$HTTP_PORT/")" = 307
curl --fail --silent "http://127.0.0.1:$HTTP_PORT/settings" \
  | grep -q 'Connect your data repository'
curl --fail --silent --output /dev/null --request POST \
  --data-urlencode "remote_url=http://127.0.0.1:$GIT_PORT/repo.git" \
  "http://127.0.0.1:$HTTP_PORT/settings"
grep -q "remote_url = \"http://127.0.0.1:$GIT_PORT/repo.git\"" \
  "$LIVE/.plainfeed/service-settings.toml"
if grep -q 'github_token' "$LIVE/.plainfeed/service-settings.toml"; then
  echo "empty Web configuration unexpectedly persisted a token" >&2
  exit 1
fi

attempt=0
until test -f "$LIVE/content/2026/07/service-daemon-content.md"; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 100 ]; then
    sed -n '1,240p' "$SERVICE_LOG" >&2
    exit 1
  fi
  sleep 0.1
done
curl --fail --silent "http://127.0.0.1:$HTTP_PORT/" \
  | grep -q 'A single WASI service pulled this entry'
test "$(git -C "$LIVE" symbolic-ref HEAD)" = refs/heads/main
test "$(git -C "$LIVE" rev-parse refs/heads/main)" = "$CONTENT_HEAD"
test -z "$(git -C "$LIVE" status --short)"

STATE="$LIVE/state/entries/20260717-wasip2-reader.toml"
if grep -q '^favorite = true$' "$STATE"; then
  FAVORITE=false
else
  FAVORITE=true
fi
curl --fail --silent --request POST --data "favorite=$FAVORITE" \
  "http://127.0.0.1:$HTTP_PORT/entries/20260717-wasip2-reader/favorite" \
  >/dev/null
# Re-saving configuration must wake the internal task immediately. This also
# keeps the integration suite fast instead of waiting for the normal idle
# publication window.
curl --fail --silent --output /dev/null --request POST \
  --data-urlencode "remote_url=http://127.0.0.1:$GIT_PORT/repo.git" \
  "http://127.0.0.1:$HTTP_PORT/settings"

attempt=0
while test "$(git --git-dir "$REMOTE" rev-parse refs/heads/main)" = "$CONTENT_HEAD"; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 400 ]; then
    sed -n '1,260p' "$SERVICE_LOG" >&2
    exit 1
  fi
  sleep 0.1
done

STATE_HEAD=$(git --git-dir "$REMOTE" rev-parse refs/heads/main)
test "$(git --git-dir "$REMOTE" rev-parse "$STATE_HEAD^")" = "$CONTENT_HEAD"
CHANGED=$(git --git-dir "$REMOTE" diff-tree \
  --no-commit-id --name-only -r "$CONTENT_HEAD" "$STATE_HEAD")
test -n "$CHANGED"
if printf '%s\n' "$CHANGED" | grep -Ev '^state/' >/dev/null; then
  echo "autonomous service publication changed a non-state path" >&2
  exit 1
fi
git --git-dir "$REMOTE" show \
  "$STATE_HEAD:state/entries/20260717-wasip2-reader.toml" \
  | grep -q "^favorite = $FAVORITE$"
test "$(find "$LIVE/.plainfeed/dirty" -type f | wc -l | tr -d ' ')" -eq 0
curl --fail --silent "http://127.0.0.1:$HTTP_PORT/health" | grep -q '^ok$'
git --git-dir "$REMOTE" fsck --full >/dev/null
git -C "$LIVE" fsck --full >/dev/null

kill "$SERVICE_PID"
wait "$SERVICE_PID" 2>/dev/null || true
SERVICE_PID=
kill "$GIT_PID"
wait "$GIT_PID" 2>/dev/null || true
GIT_PID=

echo "Plainfeed initialized through the Web UI, pulled content, and published state under $($WASMTIME_BIN -V)"

#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TEMPORARY=$(mktemp -d "${TMPDIR:-/tmp}/plainfeed-run-service.XXXXXX")
PORT=${PLAINFEED_RUN_SERVICE_PORT:-18090}
LOG="$TEMPORARY/service.log"
PID=

cleanup() {
  if [ -n "$PID" ]; then
    kill "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  rm -rf "$TEMPORARY"
}
trap cleanup EXIT INT TERM

RUSTFLAGS=--cfg=tokio_unstable cargo build --manifest-path "$ROOT/Cargo.toml" \
  -p wasmtime-run-service-experiment --target wasm32-wasip2

wasmtime run \
  -S inherit-network=y \
  --dir "$TEMPORARY::/data" \
  "$ROOT/target/wasm32-wasip2/debug/wasmtime-run-service-experiment.wasm" \
  "127.0.0.1:$PORT" /data >"$LOG" 2>&1 &
PID=$!

attempt=0
HEARTBEAT="$TEMPORARY/.plainfeed/wasmtime-run-experiment.toml"
until test -f "$HEARTBEAT"; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 50 ]; then
    sed -n '1,160p' "$LOG" >&2
    exit 1
  fi
  sleep 0.1
done

FIRST=$(sed -n 's/^ticks = //p' "$HEARTBEAT")
sleep 0.6
SECOND=$(sed -n 's/^ticks = //p' "$HEARTBEAT")
test "$SECOND" -gt "$FIRST"

curl --fail --silent "http://127.0.0.1:$PORT/health" | grep -q '^ok$'
STATUS=$(curl --fail --silent "http://127.0.0.1:$PORT/experiment/status")
case "$STATUS" in
  *"format=plainfeed.service-experiment/v1"*"ticks="*) ;;
  *) echo "service status response is incomplete" >&2; exit 1 ;;
esac

kill "$PID"
wait "$PID" 2>/dev/null || true
PID=

echo "A single WASIp2 command served HTTP while its background task advanced"

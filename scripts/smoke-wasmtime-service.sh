#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
WASMTIME_BIN=${WASMTIME_BIN:-wasmtime}
PORT=${PLAINFEED_SERVICE_SMOKE_PORT:-18092}
TEMPORARY=$(mktemp -d "${TMPDIR:-/tmp}/plainfeed-service-smoke.XXXXXX")
DATA="$TEMPORARY/data"
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

mkdir -p "$DATA"
cp -R "$ROOT/examples/data/." "$DATA/"

RUSTFLAGS=--cfg=tokio_unstable cargo build --manifest-path "$ROOT/Cargo.toml" \
  --locked --release -p plainfeed-service --target wasm32-wasip2

"$WASMTIME_BIN" run \
  -S inherit-network=y \
  -S allow-ip-name-lookup=y \
  --env PLAINFEED_REMOTE_URL= \
  --dir "$DATA::/data" \
  "$ROOT/target/wasm32-wasip2/release/plainfeed-service.wasm" \
  "127.0.0.1:$PORT" /data >"$LOG" 2>&1 &
PID=$!

attempt=0
until curl --fail --silent "http://127.0.0.1:$PORT/health" >/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 50 ]; then
    sed -n '1,200p' "$LOG" >&2
    exit 1
  fi
  sleep 0.1
done

PAGE=$(curl --fail --silent "http://127.0.0.1:$PORT/")
case "$PAGE" in
  *"Plainfeed"*"Git synchronization is viable"*"The earlier experiment proved"*) ;;
  *) echo "Axum service did not render the reader feed" >&2; exit 1 ;;
esac
case "$PAGE" in
  *"The Git experiment demonstrated"*)
    echo "Axum feed unexpectedly rendered the full entry body" >&2
    exit 1
    ;;
  *) ;;
esac
curl --fail --silent \
  "http://127.0.0.1:$PORT/entries/20260716-git-wasi" \
  | grep -q 'The Git experiment demonstrated'
curl --fail --silent "http://127.0.0.1:$PORT/style.css" | grep -q 'site-header'
curl --fail --silent --head "http://127.0.0.1:$PORT/style.css" \
  | grep -qi '^cache-control: public, max-age=3600'
curl --fail --silent --request POST --data 'favorite=false' \
  "http://127.0.0.1:$PORT/entries/20260716-git-wasi/favorite" \
  | grep -q '☆ Favorite'
curl --fail --silent --request POST \
  --data 'comment=Axum%20WASIp2%20service%20smoke' \
  "http://127.0.0.1:$PORT/entries/20260716-git-wasi/comments" \
  | grep -q 'Axum WASIp2 service smoke'

grep -q '^favorite = false$' \
  "$DATA/state/entries/20260716-git-wasi.toml"
grep -q 'body = "Axum WASIp2 service smoke"' \
  "$DATA/state/entries/20260716-git-wasi.toml"
test "$(find "$DATA/.plainfeed/dirty" -type f | wc -l | tr -d ' ')" -ge 2

kill "$PID"
wait "$PID" 2>/dev/null || true
PID=

echo "Plainfeed Axum reader and state mutations passed under $($WASMTIME_BIN -V)"

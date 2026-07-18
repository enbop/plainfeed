#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
PORT=${PLAINFEED_SMOKE_PORT:-18082}
SOURCE_DATA=${PLAINFEED_SMOKE_DATA:-$ROOT/examples/data}
DATA=$(mktemp -d "${TMPDIR:-/tmp}/plainfeed-smoke.XXXXXX")
LOG="$DATA/wasmtime.log"
SERVER_PID=

cleanup() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$DATA"
}
trap cleanup EXIT INT TERM

cp -R "$SOURCE_DATA/." "$DATA/"

cargo build \
  --manifest-path "$ROOT/Cargo.toml" \
  -p plainfeed-server \
  --target wasm32-wasip2

wasmtime serve \
  -S cli=y \
  --addr "127.0.0.1:$PORT" \
  --dir "$DATA::/data" \
  "$ROOT/target/wasm32-wasip2/debug/plainfeed_server.wasm" \
  >"$LOG" 2>&1 &
SERVER_PID=$!

attempt=0
until curl --fail --silent "http://127.0.0.1:$PORT/health" >/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 50 ]; then
    sed -n '1,160p' "$LOG"
    exit 1
  fi
  sleep 0.1
done

PAGE=$(curl --fail --silent "http://127.0.0.1:$PORT/")
case "$PAGE" in
  *"A file-backed reader running under Wasmtime"*) ;;
  *) echo "feed page did not contain the fixture entry" >&2; exit 1 ;;
esac
case "$PAGE" in
  *"Plainfeed begins with a small end-to-end slice"*) ;;
  *) echo "feed page did not render the fixture summary" >&2; exit 1 ;;
esac
case "$PAGE" in
  *"Plainfeed treats files as the source of truth"*)
    echo "feed page unexpectedly rendered the full entry body" >&2
    exit 1
    ;;
  *) ;;
esac

ARTICLE=$(curl --fail --silent \
  "http://127.0.0.1:$PORT/entries/20260717-wasip2-reader")
case "$ARTICLE" in
  *"A file-backed reader running under Wasmtime"*"Plainfeed treats files as the source of truth"*) ;;
  *) echo "entry page did not render the full Markdown article" >&2; exit 1 ;;
esac

ARTICLE_FRAGMENT=$(curl --fail --silent \
  "http://127.0.0.1:$PORT/fragments/entries/20260717-wasip2-reader")
case "$ARTICLE_FRAGMENT" in
  *"Back to feed"*"Plainfeed treats files as the source of truth"*) ;;
  *) echo "entry fragment did not render the reading view" >&2; exit 1 ;;
esac

TECHNOLOGY=$(curl --fail --silent \
  "http://127.0.0.1:$PORT/fragments/feed?channel=technology")
case "$TECHNOLOGY" in
  *"Git synchronization is viable"*) ;;
  *) echo "technology channel did not contain its fixture entry" >&2; exit 1 ;;
esac
case "$TECHNOLOGY" in
  *"A file-backed reader running under Wasmtime"*)
    echo "technology channel contained an entry from another channel" >&2
    exit 1
    ;;
  *) ;;
esac

curl --fail --silent --head "http://127.0.0.1:$PORT/" >/dev/null
curl --fail --silent --head "http://127.0.0.1:$PORT/style.css" \
  | grep -qi '^cache-control: public, max-age=3600'

# Simulate a browser abandoning an asset response, then verify the component
# remains healthy and did not panic on a closed output stream.
curl --silent "http://127.0.0.1:$PORT/vendor/htmx.min.js" | head -c 1 >/dev/null || true
curl --fail --silent "http://127.0.0.1:$PORT/health" >/dev/null
if grep -q "panicked at" "$LOG"; then
  sed -n '1,160p' "$LOG"
  exit 1
fi

mkdir -p "$DATA/.plainfeed/update.lock"
LOCKED_STATUS=$(curl --silent --output /dev/null --write-out '%{http_code}' \
  "http://127.0.0.1:$PORT/")
test "$LOCKED_STATUS" = "503"
rmdir "$DATA/.plainfeed/update.lock"

curl --fail --silent --request POST \
  "http://127.0.0.1:$PORT/entries/20260717-wasip2-reader/read" >/dev/null
curl --fail --silent --request POST --data "favorite=true" \
  "http://127.0.0.1:$PORT/entries/20260717-wasip2-reader/favorite" >/dev/null
curl --fail --silent --request POST --data "comment=Wasmtime%20smoke%20test" \
  "http://127.0.0.1:$PORT/entries/20260717-wasip2-reader/comments" >/dev/null

STATE="$DATA/state/entries/20260717-wasip2-reader.toml"
grep -q '^favorite = true$' "$STATE"
grep -q '^read_at = ' "$STATE"
grep -q 'body = "Wasmtime smoke test"' "$STATE"

echo "Plainfeed Wasmtime smoke test passed"

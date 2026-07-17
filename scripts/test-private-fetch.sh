#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 TOKEN_FILE" >&2
  exit 2
fi

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TOKEN_FILE=$1
TEMPORARY=$(mktemp -d "${TMPDIR:-/tmp}/plainfeed-private-fetch.XXXXXX")

cleanup() {
  unset PLAINFEED_GITHUB_TOKEN
  rm -rf "$TEMPORARY"
}
trap cleanup EXIT INT TERM

IFS= read -r PLAINFEED_GITHUB_TOKEN <"$TOKEN_FILE"
if [ -z "$PLAINFEED_GITHUB_TOKEN" ]; then
  echo "token file is empty" >&2
  exit 1
fi
export PLAINFEED_GITHUB_TOKEN

RUSTFLAGS="--cfg tokio_unstable" cargo build \
  --manifest-path "$ROOT/Cargo.toml" \
  -p plainfeed-git --bin plainfeed-fetch --target wasm32-wasip2

run_fetch() {
  REPOSITORY=$1
  wasmtime run \
    --env PLAINFEED_GITHUB_TOKEN \
    -S inherit-network=y \
    -S allow-ip-name-lookup=y \
    --dir "$TEMPORARY::/sync" \
    "$ROOT/target/wasm32-wasip2/debug/plainfeed-fetch.wasm" \
    https://github.com/spore-bot/plainfeed-playground.git \
    "$REPOSITORY"
}

mkdir -p "$TEMPORARY/failure/content"
printf '%s\n' "must survive a failed fetch" >"$TEMPORARY/failure/content/sentinel.md"
git init --quiet --initial-branch=main "$TEMPORARY/failure"
git -C "$TEMPORARY/failure" add content/sentinel.md
git -C "$TEMPORARY/failure" \
  -c user.name="Plainfeed Test" \
  -c user.email="test@plainfeed.invalid" \
  commit --quiet -m "test fixture"
SENTINEL_BEFORE=$(git -C "$TEMPORARY/failure" hash-object content/sentinel.md)
VALID_TOKEN=$PLAINFEED_GITHUB_TOKEN
PLAINFEED_GITHUB_TOKEN=definitely-invalid-token
export PLAINFEED_GITHUB_TOKEN
if FAILURE=$(run_fetch /sync/failure 2>&1); then
  echo "fetch unexpectedly accepted invalid credentials" >&2
  exit 1
fi
PLAINFEED_GITHUB_TOKEN=$VALID_TOKEN
export PLAINFEED_GITHUB_TOKEN
case "$FAILURE" in
  *"$VALID_TOKEN"*|*"definitely-invalid-token"*)
    echo "fetch error leaked credentials" >&2
    exit 1
    ;;
  *) ;;
esac
test "$SENTINEL_BEFORE" = "$(git -C "$TEMPORARY/failure" hash-object content/sentinel.md)"
test -z "$(git -C "$TEMPORARY/failure" status --porcelain)"

FIRST=$(run_fetch /sync/repository)
SECOND=$(run_fetch /sync/repository)
printf '%s\n' "$FIRST"
printf '%s\n' "$SECOND"

FIRST_TIP=$(printf '%s\n' "$FIRST" | sed -n 's/^remote_tip=//p')
SECOND_TIP=$(printf '%s\n' "$SECOND" | sed -n 's/^remote_tip=//p')
test -n "$FIRST_TIP"
test "$FIRST_TIP" = "$SECOND_TIP"
git --git-dir "$TEMPORARY/repository/.git" fsck --full
test "$(git --git-dir "$TEMPORARY/repository/.git" rev-parse refs/remotes/origin/main)" = "$FIRST_TIP"

echo "Plainfeed private WASIp2 fetch test passed"

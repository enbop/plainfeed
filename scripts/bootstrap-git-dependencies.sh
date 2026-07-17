#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
GITOXIDE_REVISION=7b4c806ed6175f21cc4d813ffcb197df95452197
MEMMAP2_REVISION=7163e105159cb9e95d952e42d390e36ccd83e5c1

prepare() {
  NAME=$1
  URL=$2
  DIRECTORY=$3
  REVISION=$4

  if [ ! -d "$DIRECTORY/.git" ]; then
    git clone "$URL" "$DIRECTORY"
    git -C "$DIRECTORY" checkout --detach "$REVISION"
  fi

  ACTUAL=$(git -C "$DIRECTORY" rev-parse HEAD)
  if [ "$ACTUAL" != "$REVISION" ]; then
    echo "$NAME is at $ACTUAL; expected pinned revision $REVISION" >&2
    exit 1
  fi

  if [ -n "$(git -C "$DIRECTORY" status --short)" ]; then
    echo "$NAME has unexpected local changes" >&2
    exit 1
  fi
  echo "$NAME fork checkout is ready"
}

mkdir -p "$ROOT/refs"
prepare \
  gitoxide \
  https://github.com/spore-bot/gitoxide.git \
  "$ROOT/refs/gitoxide" \
  "$GITOXIDE_REVISION"
prepare \
  memmap2 \
  https://github.com/spore-bot/memmap2.git \
  "$ROOT/refs/memmap2" \
  "$MEMMAP2_REVISION"

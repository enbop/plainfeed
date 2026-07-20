#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
GIT_WASIP2_REVISION=97ce8071124ba296a5eba827ccfc63836c58f33e
GITOXIDE_REVISION=283374581e2c8fa2fea28079e1f284a1b0c7fbfc
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
  git-wasip2 \
  https://github.com/enbop/git-wasip2.git \
  "$ROOT/refs/git-wasip2" \
  "$GIT_WASIP2_REVISION"
prepare \
  gitoxide \
  https://github.com/enbop/gitoxide.git \
  "$ROOT/refs/gitoxide" \
  "$GITOXIDE_REVISION"
prepare \
  memmap2 \
  https://github.com/enbop/memmap2-rs.git \
  "$ROOT/refs/memmap2" \
  "$MEMMAP2_REVISION"

#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
GITOXIDE_REVISION=402babdf82d6709c6a8c804e07138a8a004c54af
MEMMAP2_REVISION=7d76ad3157383db5670fd7e012f44de42aa7444b

prepare() {
  NAME=$1
  URL=$2
  DIRECTORY=$3
  REVISION=$4
  PATCH=$5

  if [ ! -d "$DIRECTORY/.git" ]; then
    git clone "$URL" "$DIRECTORY"
    git -C "$DIRECTORY" checkout --detach "$REVISION"
  fi

  ACTUAL=$(git -C "$DIRECTORY" rev-parse HEAD)
  if [ "$ACTUAL" != "$REVISION" ]; then
    echo "$NAME is at $ACTUAL; expected pinned revision $REVISION" >&2
    exit 1
  fi

  if git -C "$DIRECTORY" apply --reverse --check "$PATCH" >/dev/null 2>&1; then
    echo "$NAME compatibility patch is already applied"
    return
  fi
  if [ -n "$(git -C "$DIRECTORY" status --short)" ]; then
    echo "$NAME has unexpected local changes; refusing to apply its patch" >&2
    exit 1
  fi
  git -C "$DIRECTORY" apply "$PATCH"
  echo "$NAME compatibility patch applied"
}

mkdir -p "$ROOT/refs"
prepare \
  gitoxide \
  https://github.com/GitoxideLabs/gitoxide.git \
  "$ROOT/refs/gitoxide" \
  "$GITOXIDE_REVISION" \
  "$ROOT/experiments/git-wasi/patches/gitoxide-wasip2.patch"
prepare \
  memmap2 \
  https://github.com/RazrFalcon/memmap2-rs.git \
  "$ROOT/refs/memmap2" \
  "$MEMMAP2_REVISION" \
  "$ROOT/experiments/git-wasi/patches/memmap2-wasip2.patch"

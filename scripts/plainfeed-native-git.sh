#!/bin/sh
set -eu

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: $0 <inspect|fetch> DATA_CHECKOUT [REMOTE]" >&2
  exit 2
fi

ACTION=$1
REPOSITORY=$2
REMOTE=${3:-origin}

git -C "$REPOSITORY" rev-parse --is-inside-work-tree >/dev/null
LOCAL=$(git -C "$REPOSITORY" rev-parse refs/heads/main)

case "$ACTION" in
  inspect) ;;
  fetch)
    git -C "$REPOSITORY" fetch --no-tags "$REMOTE" \
      +refs/heads/main:refs/remotes/plainfeed-recovery/main
    ;;
  *)
    echo "usage: $0 <inspect|fetch> DATA_CHECKOUT [REMOTE]" >&2
    exit 2
    ;;
esac

git -C "$REPOSITORY" fsck --full --no-dangling >/dev/null
printf 'format=plainfeed.native-git-status/v1\n'
printf 'local_main=%s\n' "$LOCAL"
printf 'worktree_changes=%s\n' \
  "$(git -C "$REPOSITORY" status --short --untracked-files=all | wc -l | tr -d ' ')"

if git -C "$REPOSITORY" rev-parse --verify --quiet \
  refs/remotes/plainfeed-recovery/main >/dev/null; then
  FETCHED=$(git -C "$REPOSITORY" rev-parse refs/remotes/plainfeed-recovery/main)
  printf 'fetched_main=%s\n' "$FETCHED"
  if git -C "$REPOSITORY" merge-base --is-ancestor "$LOCAL" "$FETCHED"; then
    printf 'fast_forward=true\n'
  else
    printf 'fast_forward=false\n'
  fi
  git -C "$REPOSITORY" diff --name-only "$LOCAL" "$FETCHED" -- \
    content config state
else
  printf 'fetched_main=\n'
  printf 'fast_forward=unknown\n'
fi

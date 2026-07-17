#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 DATA_REPOSITORY" >&2
  exit 2
fi

REPOSITORY=$1

git -C "$REPOSITORY" rev-parse --is-inside-work-tree >/dev/null

BRANCH=$(git -C "$REPOSITORY" symbolic-ref --short HEAD)
if [ "$BRANCH" != "main" ]; then
  echo "data repository must be on main, found: $BRANCH" >&2
  exit 1
fi

test -f "$REPOSITORY/config/channels.toml"
test -d "$REPOSITORY/content"
test -d "$REPOSITORY/state/entries"
test -f "$REPOSITORY/.gitignore"

if ! git -C "$REPOSITORY" check-ignore --quiet .plainfeed/sync.toml; then
  echo ".plainfeed/ must be ignored by the data repository" >&2
  exit 1
fi

if git -C "$REPOSITORY" ls-files --error-unmatch \
  probe.txt wasi-github-smart-http-push.txt wasi-stock-reqwest-push.txt \
  >/dev/null 2>&1; then
  echo "probe-only root files must be removed from the live data repository" >&2
  exit 1
fi

if [ -n "$(git -C "$REPOSITORY" ls-files '.plainfeed/**')" ]; then
  echo ".plainfeed/ runtime data must not be tracked" >&2
  exit 1
fi

echo "Plainfeed data repository verification passed"

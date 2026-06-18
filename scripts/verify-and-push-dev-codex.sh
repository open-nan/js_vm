#!/usr/bin/env sh
set -eu

ROOT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
TARGET_BRANCH="${1:-dev/codex}"

cd "$ROOT_DIR"

sh scripts/verify.sh

if ! git diff --quiet || ! git diff --cached --quiet; then
  printf '%s\n' "working tree has uncommitted changes; commit them before pushing to ${TARGET_BRANCH}" >&2
  exit 1
fi

git push origin "HEAD:${TARGET_BRANCH}"

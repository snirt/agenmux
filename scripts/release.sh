#!/usr/bin/env bash
# Publish an existing local version-bump commit and tag.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$DIR"

branch="$(git branch --show-current)"
[ "$branch" = master ] || {
  echo 'release requires master' >&2
  exit 1
}
if ! git diff --quiet || ! git diff --cached --quiet ||
  [ -n "$(git ls-files --others --exclude-standard)" ]; then
  echo 'release requires a clean worktree' >&2
  exit 1
fi

git fetch -q origin master:refs/remotes/origin/master
version="$(bash scripts/version.sh)"
tag="v$version"
head="$(git rev-parse HEAD)"
git merge-base --is-ancestor origin/master HEAD || {
  echo 'release requires master ahead of origin/master' >&2
  exit 1
}
[ "$(git rev-list --count origin/master..HEAD)" = 1 ] || {
  echo 'release requires exactly one local bump commit' >&2
  exit 1
}
[ "$(git log -1 --pretty=%s)" = "chore: bump version to $version" ] || {
  echo 'release requires the version bump commit at HEAD' >&2
  exit 1
}
[ "$(git rev-parse "$tag^{commit}" 2>/dev/null || true)" = "$head" ] || {
  echo "release requires $tag at HEAD" >&2
  exit 1
}
if git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1; then
  echo "release tag $tag already exists on origin" >&2
  exit 1
fi

git push --atomic origin HEAD:refs/heads/master "$tag"

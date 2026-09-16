#!/bin/sh
# Tag a release: build it reproducibly, then make a signed tag whose message
# carries SHA256SUMS. The release workflow rebuilds from the tag and refuses to
# publish unless its sums equal the signed ones, so a published binary is tied
# to the release key without the key ever leaving this machine.
#
#   ci/tag-release.sh            version from Cargo.toml; does not push
set -eu
cd "$(dirname "$0")/.."
v=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -z "$(git status --porcelain --untracked-files=no)" ] || { echo "tag-release: commit first" >&2; exit 1; }
git rev-parse -q --verify "refs/tags/v$v" >/dev/null && { echo "tag-release: v$v exists" >&2; exit 1; }
ci/release.sh
{ printf '5W %s\n\n' "$v"; cat dist/SHA256SUMS; } | git tag -s "v$v" -F -
git tag -v "v$v" >/dev/null 2>&1 && echo "tag-release: signed v$v with SHA256SUMS — push: git push origin main v$v"

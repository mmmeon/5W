#!/bin/sh
# After the workflow has published a release: check its SHA256SUMS equal this
# machine's reproducible build, sign them, and attach SHA256SUMS.asc — the file
# ci/install-5w.sh verifies against the key fingerprint it pins.
#
#   ci/sign-release.sh v0.1.2     needs dist/ from ci/release.sh at that tag, gpg, gh
set -eu
cd "$(dirname "$0")/.."
tag=${1:?usage: ci/sign-release.sh v<version>}
KEY=1125DC32ECA09CA21A1810DE3491A839212CC7DB
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
curl -fsSL "https://github.com/mmmeon/5W/releases/download/$tag/SHA256SUMS" -o "$tmp/SHA256SUMS"
cmp -s "$tmp/SHA256SUMS" dist/SHA256SUMS || { echo "sign-release: published SHA256SUMS differ from dist/ — not signing" >&2; diff "$tmp/SHA256SUMS" dist/SHA256SUMS >&2; exit 1; }
gpg --batch --yes --armor --detach-sign -u "$KEY!" -o dist/SHA256SUMS.asc dist/SHA256SUMS
gpg --verify dist/SHA256SUMS.asc dist/SHA256SUMS 2>&1 | grep -q 'Good signature'
if gh auth status >/dev/null 2>&1; then
  gh release upload "$tag" dist/SHA256SUMS.asc --repo mmmeon/5W --clobber
  echo "sign-release: attached SHA256SUMS.asc to $tag"
else
  echo "sign-release: signed dist/SHA256SUMS.asc; gh is not logged in — attach it to $tag by hand or after \`gh auth login\`" >&2
fi

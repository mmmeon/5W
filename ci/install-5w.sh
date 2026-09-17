#!/bin/sh
# Install the 5w a project pins, verified. POSIX sh; needs curl and sha256sum,
# and gpg to check the signature.
#
#   sh install-5w.sh [dest-dir]        (run in the project root; default dest ~/.local/bin)
#
# The version is `requires` in .5w.toml — the one line to change to upgrade.
# The binary is checked against the release's SHA256SUMS. When the release also
# carries SHA256SUMS.asc, that file is checked to be signed by the 5W release key
# (fingerprint below); FIVEW_REQUIRE_SIGNATURE=1 makes a missing signature fatal.
set -eu
KEY_FPR=1125DC32ECA09CA21A1810DE3491A839212CC7DB   # mmmeon <si@mmmeon.com>, signing subkey
REPO=${FIVEW_REPO:-mmmeon/5W}
dest=${1:-"$HOME/.local/bin"}

# FIVEW_VERSION pins it instead, for a job that must not read the checkout's .5w.toml.
v=${FIVEW_VERSION:-$(sed -n 's/^requires *= *"\([0-9][0-9.]*\)".*/\1/p' .5w.toml 2>/dev/null | tail -n 1)}
[ -n "$v" ] || { echo "install-5w: no requires = \"x.y.z\" in ./.5w.toml" >&2; exit 1; }
target="$(uname -m)-unknown-linux-musl"
asset="5w-$v-$target"
base="https://github.com/$REPO/releases/download/v$v"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "$base/$asset" -o "$tmp/$asset"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"
( cd "$tmp" && grep "  $asset\$" SHA256SUMS | sha256sum -c - ) >&2 \
  || { echo "install-5w: $asset does not match SHA256SUMS" >&2; exit 1; }

if curl -fsSL "$base/SHA256SUMS.asc" -o "$tmp/SHA256SUMS.asc" 2>/dev/null; then
  curl -fsSL "https://raw.githubusercontent.com/$REPO/v$v/SIGNING_KEY.asc" -o "$tmp/key.asc"
  export GNUPGHOME="$tmp/gnupg"; mkdir -m 700 "$GNUPGHOME"
  gpg --batch --quiet --import "$tmp/key.asc"
  # Trust comes from the fingerprint pinned in this file, not from the key download.
  gpg --batch --status-fd 1 --verify "$tmp/SHA256SUMS.asc" "$tmp/SHA256SUMS" 2>/dev/null \
    | grep -q "^\[GNUPG:\] VALIDSIG $KEY_FPR " \
    || { echo "install-5w: SHA256SUMS is not signed by $KEY_FPR" >&2; exit 1; }
  echo "install-5w: signature ok ($KEY_FPR)" >&2
elif [ "${FIVEW_REQUIRE_SIGNATURE:-}" = 1 ]; then
  echo "install-5w: release v$v has no SHA256SUMS.asc and FIVEW_REQUIRE_SIGNATURE=1" >&2; exit 1
fi

mkdir -p "$dest"
install -m 755 "$tmp/$asset" "$dest/5w"
echo "install-5w: $("$dest/5w" --version) → $dest/5w" >&2

#!/bin/sh
# Build release binaries of 5w: static musl executables for Linux, plus SHA256SUMS.
#
#   ci/release.sh [target ...]      default: x86_64-unknown-linux-musl aarch64-unknown-linux-musl
#
# Builds inside a pinned rust:<version>-alpine container (podman or docker), so the
# host needs no toolchain and every machine produces the same bytes: the compiler
# is pinned, paths are remapped, the timestamp comes from the commit, and
# Cargo.lock is enforced. FIVEW_NATIVE=1 builds with the host's cargo instead,
# for targets whose standard library it already has.
#
# Output: dist/5w-<version>-<target> and dist/SHA256SUMS. The binaries are what
# the CI wrappers download: FIVEW_URL = the asset's URL, FIVEW_SHA256 = its line.
set -eu

cd "$(dirname "$0")/.."
RUST_VERSION=1.98.1
IMAGE="docker.io/library/rust:${RUST_VERSION}-alpine"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
commit=$(git rev-parse --short=12 HEAD)
epoch=$(git log -1 --format=%ct)
[ -n "$(git status --porcelain --untracked-files=no)" ] && commit="$commit-dirty"
targets=${*:-x86_64-unknown-linux-musl aarch64-unknown-linux-musl}

engine=""
if [ "${FIVEW_NATIVE:-}" != 1 ]; then
  for e in podman docker; do command -v "$e" >/dev/null 2>&1 && engine=$e && break; done
  [ -n "$engine" ] || { echo "release: need podman or docker (or FIVEW_NATIVE=1)" >&2; exit 1; }
fi

mkdir -p dist
: > dist/SHA256SUMS.tmp
for target in $targets; do
  echo "release: $version ($commit) for $target"
  # Everything the build needs, identical inside the container and out.
  script="
    set -eu
    rustup target add $target >/dev/null 2>&1 || true
    export SOURCE_DATE_EPOCH=$epoch CARGO_INCREMENTAL=0 FIVEW_COMMIT=$commit
    export RUSTFLAGS='--remap-path-prefix=/src=. -C strip=symbols -C target-feature=+crt-static -C link-self-contained=yes -C linker=rust-lld'
    cargo build --release --locked --target $target --target-dir /tmp/target
    cp /tmp/target/$target/release/5w /src/dist/5w-$version-$target
  "
  if [ -n "$engine" ]; then
    "$engine" run --rm -v "$PWD:/src:Z" -w /src \
      -v fivew-cargo-registry:/usr/local/cargo/registry \
      "$IMAGE" sh -c "$script"
  else
    sh -c "$(echo "$script" | sed "s#/src#$PWD#g; s#/tmp/target#$PWD/target#g")"
  fi
done

# Test what ships: the whole suite against the artifact this host can run.
host="$(uname -m)-unknown-linux-musl"
if [ -f "dist/5w-$version-$host" ] && [ "${FIVEW_SKIP_TESTS:-}" != 1 ]; then
  echo "release: testing dist/5w-$version-$host"
  mkdir -p dist/test && cp "dist/5w-$version-$host" dist/test/5w
  FIVEW_TEST_BIN="$PWD/dist/test/5w" cargo test --locked --quiet --test flow
  rm -rf dist/test
fi

( cd dist && for f in 5w-"$version"-*; do sha256sum "$f"; done ) > dist/SHA256SUMS.tmp
mv dist/SHA256SUMS.tmp dist/SHA256SUMS
cat dist/SHA256SUMS

#!/usr/bin/env bash
# Builds dist/*.deb and dist/*.rpm. Compiles in Debian bookworm so the
# binaries only need glibc >= 2.36 (Ubuntu 24.04+, Fedora 37+).
set -euo pipefail
cd "$(dirname "$0")/.."

RUST_IMAGE=rust:1.90-bookworm
NFPM_IMAGE=goreleaser/nfpm:v2.48.0
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
# Run as the calling user so target/ and dist/ do not end up owned by root.
RUN=(docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/src" -w /src)

mkdir -p target/docker/cargo dist
"${RUN[@]}" -e HOME=/tmp -e CARGO_HOME=/src/target/docker/cargo \
    -e CARGO_TARGET_DIR=/src/target/docker \
    "$RUST_IMAGE" cargo build --release --locked

for packager in deb rpm; do
    "${RUN[@]}" -e VERSION="$VERSION" "$NFPM_IMAGE" \
        package --config packaging/nfpm.yaml --packager "$packager" --target dist/
done
ls -l dist

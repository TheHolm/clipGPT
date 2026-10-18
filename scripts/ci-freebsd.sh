#!/bin/sh
# Cross-compile clip_gpt for FreeBSD and build a .pkg from it, using a
# sysroot assembled by the caller (base.txz libc/CRT files plus the
# alsa-lib/openssl native dependency closure fetched by
# fetch-freebsd-deps.py - see .woodpecker/release.yaml for how the sysroot
# at $1 is put together before this script runs).
#
# Usage: ci-freebsd.sh <sysroot> <stage-dir> <dist-dir> [deps-file]
#
# No FreeBSD host or `pkg` binary is used anywhere in this script - see
# scripts/build-freebsd-pkg.py for how the .pkg is written directly.
set -eu

SYSROOT=$1
STAGE_DIR=$2
DIST_DIR=$3
DEPS_FILE=${4:-}

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)

rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR/usr/local/bin" "$STAGE_DIR/usr/local/share/man/man1" "$DIST_DIR"

export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
export PKG_CONFIG_LIBDIR="$SYSROOT/usr/local/libdata/pkgconfig:$SYSROOT/usr/local/lib/pkgconfig"
export CARGO_TARGET_X86_64_UNKNOWN_FREEBSD_LINKER=clang
export CARGO_TARGET_DIR="$REPO_ROOT/target/freebsd"
export RUSTFLAGS="-C link-arg=--target=x86_64-unknown-freebsd15 -C link-arg=--sysroot=$SYSROOT -C link-arg=-fuse-ld=lld -C link-arg=-B$SYSROOT/usr/lib"

cargo build --release --target x86_64-unknown-freebsd --manifest-path "$REPO_ROOT/Cargo.toml"

cp "$CARGO_TARGET_DIR/x86_64-unknown-freebsd/release/clip_gpt" "$STAGE_DIR/usr/local/bin/clip_gpt"
chmod 755 "$STAGE_DIR/usr/local/bin/clip_gpt"
# FreeBSD packages live under /usr/local, man pages gzipped (-n: no name or
# timestamp in the header, so the build stays reproducible).
gzip -9nc "$REPO_ROOT/man/clip_gpt.1" > "$STAGE_DIR/usr/local/share/man/man1/clip_gpt.1.gz"
chmod 644 "$STAGE_DIR/usr/local/share/man/man1/clip_gpt.1.gz"

DEPS_ARGS=""
if [ -n "$DEPS_FILE" ] && [ -f "$DEPS_FILE" ]; then
    DEPS_ARGS="--deps-file $DEPS_FILE"
fi

# shellcheck disable=SC2086
python3 "$SCRIPT_DIR/build-freebsd-pkg.py" \
    --name clip_gpt \
    --origin sysutils/clip_gpt \
    --version "$VERSION" \
    --stage-root "$STAGE_DIR" \
    --output "$DIST_DIR/clip_gpt-$VERSION-freebsd-amd64.pkg" \
    --comment "Clipboard spelling/grammar fixer via an LLM" \
    --www "https://github.com/TheHolm/clipGPT" \
    --desc "Grabs clipboard text, sends it to an OpenAI-compatible LLM API, and pastes the response back onto the clipboard. Meant to be bound to an OS-level hotkey or macro key." \
    $DEPS_ARGS

# Build-time smoke test: confirm the binary and man page landed in the package.
zstd -dc "$DIST_DIR/clip_gpt-$VERSION-freebsd-amd64.pkg" | tar -tf - | grep -qF 'usr/local/bin/clip_gpt'
zstd -dc "$DIST_DIR/clip_gpt-$VERSION-freebsd-amd64.pkg" | tar -tf - | grep -qF 'usr/local/share/man/man1/clip_gpt.1.gz'

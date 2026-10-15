#!/bin/bash
set -ex

export ASAN_OPTIONS="detect_odr_violation=0 detect_leaks=0"

TARGET="x86_64-unknown-linux-gnu"

# Sanitizers run against the pure-Rust crates only. The FFmpeg crates
# pull in FFmpeg's C libraries, which aren't instrumented and would
# generate uninteresting noise (especially MSAN/TSAN, which would need a
# fully-instrumented FFmpeg + libc rebuild).
PKG=("-p" "mediaspine" "-p" "mediadecode" "-p" "mediaencode")

# Run address sanitizer
RUSTFLAGS="-Z sanitizer=address" \
cargo test "${PKG[@]}" --tests --target "$TARGET" --all-features

# Run leak sanitizer
RUSTFLAGS="-Z sanitizer=leak" \
cargo test "${PKG[@]}" --tests --target "$TARGET" --all-features

# Run memory sanitizer (requires -Zbuild-std for instrumented std)
RUSTFLAGS="-Z sanitizer=memory" \
cargo -Zbuild-std test "${PKG[@]}" --tests --target "$TARGET" --all-features

# Run thread sanitizer (requires -Zbuild-std for instrumented std)
RUSTFLAGS="-Z sanitizer=thread" \
cargo -Zbuild-std test "${PKG[@]}" --tests --target "$TARGET" --all-features

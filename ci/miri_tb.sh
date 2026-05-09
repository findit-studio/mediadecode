#!/bin/bash
set -e

if [ -z "$1" ]; then
  echo "Error: TARGET is not provided"
  exit 1
fi

TARGET="$1"

# Install cross-compilation toolchain on Linux
if [ "$(uname)" = "Linux" ]; then
  case "$TARGET" in
    aarch64-unknown-linux-gnu)
      sudo apt-get update && sudo apt-get install -y gcc-aarch64-linux-gnu
      ;;
    i686-unknown-linux-gnu)
      sudo apt-get update && sudo apt-get install -y gcc-multilib
      ;;
    powerpc64-unknown-linux-gnu)
      sudo apt-get update && sudo apt-get install -y gcc-powerpc64-linux-gnu
      ;;
    s390x-unknown-linux-gnu)
      sudo apt-get update && sudo apt-get install -y gcc-s390x-linux-gnu
      ;;
    riscv64gc-unknown-linux-gnu)
      sudo apt-get update && sudo apt-get install -y gcc-riscv64-linux-gnu
      ;;
  esac
fi

rustup toolchain install nightly --component miri
rustup override set nightly
cargo miri setup

export MIRIFLAGS="-Zmiri-strict-provenance -Zmiri-disable-isolation -Zmiri-symbolic-alignment-check -Zmiri-tree-borrows"

# `mediadecode-ffmpeg` calls FFmpeg's C library through FFI, which
# Miri can't interpret. Run Miri only against the pure-Rust core
# crate `mediadecode`.
cargo miri test -p mediadecode --all-targets --target "$TARGET"

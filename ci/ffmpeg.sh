#!/bin/bash
# Builds the pinned FFmpeg into a prefix, as one of two variants.
#
#   full  GPL, with libx264, libx265, libmp3lame and libvpx, and the
#         `ffmpeg` program: what the integration tests' fixtures are made
#         with, and what the write adapter's `gpl` lane runs against.
#   lgpl  no GPL code and no programs: the libraries a build that must
#         not depend on GPL code links, where the refuse-by-name law runs.
#
# Homebrew has no versioned formula for FFmpeg 9 (`ffmpeg@8` is the
# newest), and its unversioned `ffmpeg` moves with every release, so CI
# builds the release tarball itself, pinned by version and checksum, and
# caches the result (see `.github/actions/ffmpeg`).
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <full|lgpl> <prefix>" >&2
  exit 1
fi
VARIANT="$1"
PREFIX="$2"

VERSION="9.0.1"
# The release tarball's SHA-256, as Homebrew's `ffmpeg` 9.0.1 formula
# records it.
SHA256="cf38e0e28c7e5605942c4a77755349b0145804a397af37eb1fb4c77cb237f635"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

curl -fsSL --retry 3 "https://ffmpeg.org/releases/ffmpeg-${VERSION}.tar.xz" -o "$work/ffmpeg.tar.xz"
echo "${SHA256}  $work/ffmpeg.tar.xz" | shasum -a 256 -c -
tar -xJf "$work/ffmpeg.tar.xz" -C "$work"
cd "$work/ffmpeg-${VERSION}"

common=(
  --prefix="$PREFIX"
  --enable-shared
  --disable-static
  --enable-pthreads
  --disable-doc
)
if [ "$(uname)" = "Darwin" ]; then
  common+=(--enable-videotoolbox --enable-audiotoolbox)
fi

case "$VARIANT" in
  full)
    ./configure "${common[@]}" \
      --enable-gpl \
      --enable-libx264 \
      --enable-libx265 \
      --enable-libmp3lame \
      --enable-libvpx \
      --disable-ffplay
    ;;
  lgpl)
    ./configure "${common[@]}" \
      --disable-gpl \
      --disable-programs
    ;;
  *)
    echo "unknown variant: $VARIANT (expected full or lgpl)" >&2
    exit 1
    ;;
esac

if [ "$(uname)" = "Darwin" ]; then
  jobs="$(sysctl -n hw.ncpu)"
else
  jobs="$(nproc)"
fi
make -j"$jobs"
make install

#!/usr/bin/env bash
# Builds LibRaw for macOS so the `rawlib` crate links on a Mac.
#
# The crate ships LibRaw 0.22.2 headers with Linux (ELF) and Windows (MSVC)
# archives only, and its build script links `-lraw -lstdc++`, which is a
# GNU-toolchain assumption: macOS has no libstdc++ (it ships libc++). So
# this script, run before `cargo build` on the macOS runners:
#
#   1. builds LibRaw 0.22.2 from source as a static library for the target
#      architecture (native arm64, or x86_64 for the Intel .dmg);
#   2. writes a `libraw.pc` so the crate's build script takes its
#      "system libraw via pkg-config" path, which links `-lraw` without
#      pointing the linker at the bundled ELF archives;
#   3. provides an empty `libstdc++.a` so `-lstdc++` resolves to nothing —
#      LibRaw's C++ symbols come from libc++, which the link already has.
#
# It prints the environment the build step needs; the workflow evals it.
#
#   scripts/ci/macos-libraw.sh [x86_64|arm64]
set -euo pipefail

arch="${1:-$(uname -m)}"
version="0.22.2"
prefix="${RUNNER_TEMP:-/tmp}/libraw-${arch}"
src="${RUNNER_TEMP:-/tmp}/libraw-src-${arch}"

# The crate's build script asks pkg-config whether libraw exists.
command -v pkg-config >/dev/null 2>&1 || brew install pkgconf >/dev/null

# configure only needs a host triple when cross-building.
host_flag=""
if [ "${arch}" != "$(uname -m)" ]; then host_flag="--host=${arch}-apple-darwin"; fi

if [ ! -f "${prefix}/lib/libraw.a" ]; then
  rm -rf "${src}" && mkdir -p "${src}"
  curl -fsSL "https://www.libraw.org/data/LibRaw-${version}.tar.gz" | tar -xz -C "${src}" --strip-components=1
  (
    cd "${src}"
    # Minimal: no LCMS, JasPer, OpenMP or libjpeg, so nothing else has to be
    # bundled into the .app. RAW decoding itself needs none of them.
    ./configure \
      --prefix="${prefix}" \
      --host="${arch}-apple-darwin" \
      --disable-shared --enable-static \
      --disable-lcms --disable-jasper --disable-openmp --disable-jpeg \
      --disable-examples \
      CC="clang -arch ${arch} -mmacosx-version-min=11.0" \
      CXX="clang++ -arch ${arch} -mmacosx-version-min=11.0" >/dev/null
    make -j"$(sysctl -n hw.ncpu)" >/dev/null
    make install >/dev/null
  )
fi

# An empty archive named libstdc++.a: satisfies `-lstdc++` with nothing.
if [ ! -f "${prefix}/lib/libstdc++.a" ]; then
  echo 'void skwad_libstdcxx_stub(void) {}' > "${prefix}/stub.c"
  clang -arch "${arch}" -c "${prefix}/stub.c" -o "${prefix}/stub.o"
  ar rcs "${prefix}/lib/libstdc++.a" "${prefix}/stub.o"
fi

mkdir -p "${prefix}/lib/pkgconfig"
cat > "${prefix}/lib/pkgconfig/libraw.pc" <<PC
prefix=${prefix}
libdir=\${prefix}/lib
includedir=\${prefix}/include
Name: libraw
Description: LibRaw ${version}, static, built for ${arch}
Version: ${version}
Libs: -L\${libdir} -lraw
Cflags: -I\${includedir}
PC

echo "built LibRaw ${version} for ${arch} in ${prefix}" >&2
# What the build needs: pkg-config must find libraw.pc, and the linker must
# find libraw.a and the libstdc++.a stub.
echo "PKG_CONFIG_PATH=${prefix}/lib/pkgconfig"
echo "LIBRARY_PATH=${prefix}/lib"

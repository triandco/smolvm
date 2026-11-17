#!/bin/sh
# build-patched-dist.sh <platform> <version-suffix> <out-dir>: repackages the pinned
# upstream release tarball for <platform> (darwin-arm64 or linux-x86_64).
# On darwin-arm64 it builds the patched smolvm-bin and libkrun and puts them in place
# of the originals. The patch is macOS-only, so linux-x86_64 keeps the upstream binaries.
# Everything else (libkrunfw, the GPU libraries, agent-rootfs, templates) comes from
# the upstream release. agent-rootfs ships as agent-rootfs.tar.gz, which smolvm
# extracts on first use, because it holds absolute symlinks that tools such as mise
# refuse to install.
set -eu

upstream=1.35.1
platform=$1
suffix=$2
out=$(mkdir -p "$3" && cd "$3" && pwd)
src=$(cd "$(dirname "$0")/.." && pwd)
work=$src/target/patched-dist
base=smolvm-$upstream-$platform
name=smolvm-$upstream-$suffix-$platform

case $platform in
darwin-arm64) sha256=962142af8820f2c7dc5a03d755ed1fbecea418201ff24e4bad48d2cdd313de0e ;;
linux-x86_64) sha256=1c9377f6ada3c7c6753efada90595d5f44eb7a900ca0e4f8b786815b84fd22be ;;
*) echo "unknown platform: $platform" >&2 && exit 1 ;;
esac

mkdir -p "$work"
if [ ! -f "$work/$base.tar.gz" ]; then
    curl -fL -o "$work/$base.tar.gz" \
        "https://github.com/smol-machines/smolvm/releases/download/v$upstream/$base.tar.gz"
fi
echo "$sha256  $work/$base.tar.gz" | shasum -a 256 -c -
rm -rf "${work:?}/$base" "${work:?}/$name"
tar -xzf "$work/$base.tar.gz" -C "$work"

build_darwin() {
    # Homebrew's virglrenderer lacks virgl_renderer_resource_get_map_ptr, so libkrun
    # links against the release's own copies of virglrenderer and epoxy.
    link=$work/link
    mkdir -p "$link/lib" "$link/pkgconfig"
    ln -sf "$work/$base/lib/libvirglrenderer.1.dylib" "$link/lib/libvirglrenderer.dylib"
    ln -sf "$work/$base/lib/libepoxy.0.dylib" "$link/lib/libepoxy.dylib"
    pkgconfig() {
        cat >"$link/pkgconfig/$1.pc" <<EOF
libdir=$link/lib
includedir=$(brew --prefix "$2")/include
Name: $1
Description: copy from the smolvm release
Version: 1.3.0
Libs: -L\${libdir} -l$1
Cflags: -I\${includedir}
EOF
    }
    pkgconfig virglrenderer virglrenderer
    pkgconfig epoxy libepoxy

    (cd "$src/libkrun" &&
        PKG_CONFIG_PATH=$link/pkgconfig LIBCLANG_PATH=$(brew --prefix llvm)/lib \
            make BLK=1 NET=1 GPU=1 INPUT=1)
    (cd "$src" && LIBKRUN_BUNDLE=$work/$base/lib cargo build --release --bin smolvm)

    cp "$src/target/release/smolvm" "$work/$base/smolvm-bin"
    cp -L "$src/libkrun/target/release/libkrun.dylib" "$work/$base/lib/libkrun.dylib"
    for lib in "$work/$base"/lib/*.dylib; do
        [ -L "$lib" ] || codesign --force --sign - "$lib"
    done
    codesign --force --sign - --entitlements "$src/smolvm.entitlements" "$work/$base/smolvm-bin"
}

if [ "$platform" = darwin-arm64 ]; then
    build_darwin
fi
mv "$work/$base" "$work/$name"

# The rootfs entries stay owned by root, as in the upstream tarball.
if tar --version | grep -q GNU; then root="--owner=0 --group=0"; else root="--uid 0 --gid 0"; fi
# shellcheck disable=SC2086
COPYFILE_DISABLE=1 tar $root -czf "$work/$name/agent-rootfs.tar.gz" -C "$work/$name/agent-rootfs" .
rm -rf "${work:?}/$name/agent-rootfs"

outside=$(find "$work/$name" -type l -exec readlink {} \; | grep -E '^/|(^|/)\.\.(/|$)' || true)
if [ -n "$outside" ]; then
    echo "symlinks point outside the distribution:" >&2
    echo "$outside" >&2
    exit 1
fi

(cd "$work/$name" && rm -f checksums.txt &&
    find . -type f ! -name checksums.txt | sort | xargs shasum -a 256 >checksums.txt)

COPYFILE_DISABLE=1 tar -czf "$out/$name.tar.gz" -C "$work" "$name"
(cd "$out" && shasum -a 256 "$name.tar.gz" >"$name.tar.gz.sha256")
echo "built $out/$name.tar.gz"

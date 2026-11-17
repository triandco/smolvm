#!/bin/sh
# build-patched-dist.sh <version-suffix> <out-dir>: builds the patched smolvm-bin and
# libkrun, and puts them in the upstream release tarball in place of the originals.
# Everything else (libkrunfw, the GPU libraries, agent-rootfs, templates) comes from
# the pinned upstream release.
set -eu

upstream=1.35.1
sha256=962142af8820f2c7dc5a03d755ed1fbecea418201ff24e4bad48d2cdd313de0e
suffix=$1
out=$(mkdir -p "$2" && cd "$2" && pwd)
src=$(cd "$(dirname "$0")/.." && pwd)
work=$src/target/patched-dist
base=smolvm-$upstream-darwin-arm64
name=smolvm-$upstream-$suffix-darwin-arm64

mkdir -p "$work"
if [ ! -f "$work/$base.tar.gz" ]; then
    curl -fL -o "$work/$base.tar.gz" \
        "https://github.com/smol-machines/smolvm/releases/download/v$upstream/$base.tar.gz"
fi
echo "$sha256  $work/$base.tar.gz" | shasum -a 256 -c -
rm -rf "${work:?}/$base"
tar -xzf "$work/$base.tar.gz" -C "$work"

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

rm -rf "${work:?}/$name"
mv "$work/$base" "$work/$name"
cp "$src/target/release/smolvm" "$work/$name/smolvm-bin"
cp -L "$src/libkrun/target/release/libkrun.dylib" "$work/$name/lib/libkrun.dylib"
for lib in "$work/$name"/lib/*.dylib; do
    [ -L "$lib" ] || codesign --force --sign - "$lib"
done
codesign --force --sign - --entitlements "$src/smolvm.entitlements" "$work/$name/smolvm-bin"
(cd "$work/$name" && rm -f checksums.txt &&
    find . -type f ! -path './agent-rootfs/*' | sort | xargs shasum -a 256 >checksums.txt)

tar -czf "$out/$name.tar.gz" -C "$work" "$name"
(cd "$out" && shasum -a 256 "$name.tar.gz" >"$name.tar.gz.sha256")
echo "built $out/$name.tar.gz"

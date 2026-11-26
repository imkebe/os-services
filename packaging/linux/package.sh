#!/usr/bin/env bash
# Builds the AppImage from the release build, its libipkvm-kms.so and the
# staged libdrmtap.
# `cargo xtask package` runs it in two steps and writes the notices into the
# AppDir between them:
#   package.sh deploy    copies the program, libdrmtap and the libraries they
#                        need into $RELEASE_DIR/appdir
#   package.sh appimage  packs $RELEASE_DIR/appdir into an AppImage in
#                        $RELEASE_DIR/bundle/appimage
# RELEASE_DIR is the cargo release folder (target/release, or
# target/<triple>/release for a --target build), TARGET_ARCH the
# architecture of the build as Rust names it (x86_64, aarch64, arm). OBJDUMP
# reads the bundled files; a cross build passes the target's objdump.
set -euo pipefail
umask 022
cd "$(dirname "$0")/../.."
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
release="${RELEASE_DIR:-$PWD/target/release}"
output="$release/bundle"
appdir="$release/appdir"
bundled="$appdir/usr/lib/ipkvm-os-services"
# linuxdeploy's name for the architecture, and the AppImage's.
case "${TARGET_ARCH:-$(uname -m)}" in
  x86_64) tools=x86_64 label=amd64 ;;
  aarch64) tools=aarch64 label=arm64 ;;
  arm|armv7l) tools=armhf label=armhf ;;
  *) echo "no AppImage tools for ${TARGET_ARCH:-$(uname -m)}" >&2; exit 2 ;;
esac
cache="${XDG_CACHE_HOME:-$HOME/.cache}/ipkvm-os-services-packaging/$tools"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

case "${1:-}" in
  deploy)
    rm -rf "$appdir"
    mkdir -p "$bundled"
    install -m755 packaging/linux/drmtap/drmtap-helper "$bundled/"
    library=$(find packaging/linux/drmtap -name "libdrmtap.so.0.*" | head -1)
    install -m644 "$library" "$bundled/libdrmtap.so.0"
    # The encoders and FFmpeg, loaded only for video (build.rs).
    install -m644 "$release/libipkvm-kms.so" "$bundled/"
    mkdir -p "$cache"
    # Each tool is a fixed release with a fixed SHA-256 per architecture,
    # and a cached copy is checked too. The aarch64 and armhf linuxdeploy
    # sums are the ones GitHub lists for the release; the plugin's release
    # lists none, so its sums are of the files downloaded on 2026-11-20.
    case "$tools" in
      x86_64) sums="c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d 992d502a248e14ab185448ddf6f6e7d25558cb84d4623c354c3af350c25fccb3" ;;
      aarch64) sums="620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff 83c292149274965a865dcd44c135cfca8ba28c6b7de3eb628d4b8b5f248af17c" ;;
      armhf) sums="e359161979fa4bee50b92ce7102fb510299caebf34f711d983fba7a8f4bb1c2e c15dc8fb0478c4656a81d664176786e1a42a70746095ca66e1ca1d59ad4b74ea" ;;
    esac
    read -r linuxdeploy_sum plugin_sum <<< "$sums"
    for tool in \
      "linuxdeploy 1-alpha-20251107-1 $linuxdeploy_sum" \
      "linuxdeploy-plugin-appimage 1-alpha-20250213-1 $plugin_sum"; do
      read -r name tag sha256 <<< "$tool"
      file="$cache/$name-$tools.AppImage"
      if ! echo "$sha256  $file" | sha256sum --check --status 2>/dev/null; then
        curl -fL --retry 3 "https://github.com/linuxdeploy/$name/releases/download/$tag/$name-$tools.AppImage" -o "$file"
        echo "$sha256  $file" | sha256sum --check --quiet
      fi
      chmod +x "$file"
    done
    ;;
  appimage)
    mkdir -p "$output/appimage"
    ;;
  *)
    echo "usage: $0 deploy|appimage" >&2
    exit 2
    ;;
esac

cat > "$work/ipkvm-os-services.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=IP-KVM OS Services
Exec=ipkvm-os-services
Icon=ipkvm-os-services
Categories=Utility;
Terminal=false
EOF
export APPIMAGE_EXTRACT_AND_RUN=1
# A foreign architecture's AppImage runs through binfmt emulation only
# without the AppImage magic ("AI" 2) in the padding of its ELF header,
# which binfmt rules require to be zero, so it runs from copies without it.
tooldir="$cache"
case "$(uname -m)/$tools" in
  x86_64/x86_64 | aarch64/aarch64 | armv7l/armhf) ;;
  *)
    tooldir="$cache/binfmt"
    mkdir -p "$tooldir"
    for file in "$cache"/*.AppImage; do
      cp "$file" "$tooldir/"
      printf '\0\0\0' | dd of="$tooldir/$(basename "$file")" bs=1 seek=8 conv=notrunc status=none
    done
    ;;
esac
export PATH="$tooldir:$PATH"
# appimagetool takes the runtime's architecture from ARCH.
export ARCH="$tools"
export OUTPUT="$output/appimage/IP-KVM-OS-Services_${version}_${label}.AppImage"
arguments=(
  --appdir "$appdir" --executable "$release/ipkvm-os-services"
  --deploy-deps-only "$bundled"
  --desktop-file "$work/ipkvm-os-services.desktop"
  --icon-file packaging/icons/icon.png --icon-filename ipkvm-os-services
  # The host supplies VAAPI and EGL/GLES alongside its GPU driver. Only
  # libipkvm-kms.so links them, and the program loads it for video alone,
  # so the program starts without them.
  --exclude-library 'libva*' --exclude-library 'libEGL*'
  --exclude-library 'libGLES*'
)
if [[ "$1" == appimage ]]; then
  arguments+=(--output appimage)
fi
"$tooldir/linuxdeploy-$tools.AppImage" --appimage-extract-and-run "${arguments[@]}"

# The AppImage runs on glibc 2.35 and newer (docs/install.md). A bundled
# library that needs a newer one would raise that minimum, so the build
# fails instead. Weak symbols do not count.
minimum=2.35
newest=$({ find "$appdir/usr" -type f \( -name '*.so*' -o -perm -u+x \) -exec "${OBJDUMP:-objdump}" -T {} + 2>/dev/null || true; } |
    awk '$2 != "w" && match($0, /GLIBC_2\.[0-9]+/) { print substr($0, RSTART + 6, RLENGTH - 6) }' |
    sort -V | tail -1)
if [[ "$(printf '%s\n%s\n' "$newest" "$minimum" | sort -V | tail -1)" != "$minimum" ]]; then
    echo "the AppImage needs glibc $newest, more than the minimum $minimum in docs/install.md" >&2
    exit 1
fi

#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
source_dir="$root/dist/${TARGET_TRIPLE:?}"
name="APEX-TRACE-${VERSION:?}-${PLATFORM:?}"
assets="$root/release-assets"
sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1"
  else
    shasum -a 256 "$1"
  fi
}
if command -v python >/dev/null 2>&1; then
  python_bin=python
else
  python_bin=python3
fi
mkdir -p "$assets"
binary="car-logger-gui"; [[ "$PLATFORM" == windows-* ]] && binary="car-logger-gui.exe"
(cd "$source_dir" && "$python_bin" -m zipfile -c "$assets/$name.zip" "$binary")
(cd "$assets" && sha256_file "$name.zip" > "$name.zip.sha256")
if [[ "$PLATFORM" == linux-* ]]; then
  appdir="$root/target/APEX-TRACE.AppDir"; rm -rf "$appdir"; mkdir -p "$appdir/usr/bin" "$appdir/usr/share/icons/hicolor/scalable/apps"
  cp "$source_dir/$binary" "$appdir/usr/bin/"
  cp "$root/apps/car-logger-gui/resources/icons/apex-trace.svg" "$appdir/apex-trace.svg"
  cp "$appdir/apex-trace.svg" "$appdir/usr/share/icons/hicolor/scalable/apps/"
  printf '%s\n' '[Desktop Entry]' 'Type=Application' 'Name=APEX TRACE' 'Exec=car-logger-gui' 'Icon=apex-trace' 'Categories=Utility;' > "$appdir/apex-trace.desktop"
  printf '%s\n' '#!/bin/sh' 'HERE="$(dirname "$(readlink -f "$0")")"' 'exec "$HERE/usr/bin/car-logger-gui" "$@"' > "$appdir/AppRun"; chmod +x "$appdir/AppRun"
  tool="${APPIMAGETOOL:-$root/target/appimagetool}"
  if [[ ! -x "$tool" ]]; then curl -fsSL -o "$tool" https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage; chmod +x "$tool"; fi
  ARCH=x86_64 "$tool" "$appdir" "$assets/$name.AppImage"
  (cd "$assets" && sha256_file "$name.AppImage" > "$name.AppImage.sha256")
elif [[ "$PLATFORM" == windows-* ]]; then
  cp "$source_dir/$binary" "$assets/$name.exe"
elif [[ "$PLATFORM" == macos-* ]]; then
  tar -C "$source_dir" -czf "$assets/$name.tar.gz" .
fi

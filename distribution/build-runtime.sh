#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
source "$root/distribution/versions.env"
target="${TARGET_TRIPLE:?set TARGET_TRIPLE}"
out="$root/dist/$target"
python="${PYTHON_BIN:-python3.11}"

rm -rf "$out"
mkdir -p "$out/runtime" "$out/licenses"
"$python" -c "import sys; assert sys.version.split()[0] == '$PYTHON_VERSION', sys.version"
"$python" -m venv "$out/runtime/python"
py="$out/runtime/python/bin/python"
[[ "$target" == *windows* ]] && py="$out/runtime/python/Scripts/python.exe"
if [[ "$target" == aarch64-unknown-linux-gnu && -n "${TENSORFLOW_WHEEL:-}" ]]; then
  "$py" -m pip install --disable-pip-version-check --no-cache-dir "$TENSORFLOW_WHEEL"
fi
"$py" -m pip install --disable-pip-version-check --no-cache-dir -r "$root/distribution/requirements-$target.txt"
"$py" -m pip install --no-deps "$root/python/ai_worker"
cargo build --release --locked --target "$target"
cp -R "$root/python/ai_worker/car_logger_ai_worker" "$root/python/ai_worker/run_worker.py" "$out/runtime/"
"$py" -m pip freeze --all > "$out/PYTHON-DEPENDENCIES.txt"
cp "$root/target/$target/release/car-logger-gui" "$out/" 2>/dev/null || cp "$root/target/$target/release/car-logger-gui.exe" "$out/"
"$root/distribution/write-manifest.sh" "$out" "$target"

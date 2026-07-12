#!/usr/bin/env bash
set -euo pipefail
out="${1:?artifact directory}"; target="${2:?target}"; root="$(cd "$(dirname "$0")/.." && pwd)"
source "$root/distribution/versions.env"
sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1"
  else
    shasum -a 256 "$1"
  fi
}
{
  echo "target=$target"
  echo "python=$PYTHON_VERSION"
  echo "tensorflow=$TENSORFLOW_VERSION"
  echo "keras=$KERAS_VERSION"
  echo "worker_protocol=$WORKER_PROTOCOL_VERSION"
  echo "model_structure=$MODEL_STRUCTURE_VERSION"
  echo "feature_schema=$FEATURE_SCHEMA_VERSION"
  echo "build_commit=$(git -C "$root" rev-parse HEAD)"
  echo "dependencies=distribution/requirements-$target.txt,Cargo.lock"
  echo "licenses=distribution/THIRD_PARTY_LICENSES.md"
} > "$out/BUILD-MANIFEST.txt"
(
  cd "$out"
  while IFS= read -r -d '' file; do
    sha256_file "$file"
  done < <(find . -type f ! -name SHA256SUMS -print0 | sort -z)
) > "$out/SHA256SUMS"

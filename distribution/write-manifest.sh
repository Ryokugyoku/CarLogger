#!/usr/bin/env bash
set -euo pipefail
out="${1:?artifact directory}"; target="${2:?target}"; root="$(cd "$(dirname "$0")/.." && pwd)"
source "$root/distribution/versions.env"
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
(cd "$out" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 shasum -a 256 > SHA256SUMS)

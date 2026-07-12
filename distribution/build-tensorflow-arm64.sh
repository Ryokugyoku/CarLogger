#!/usr/bin/env bash
set -euo pipefail
# Runs only in the release builder; Raspberry Pi users receive the resulting wheel.
# Pin the TensorFlow source and Bazel container by digest for reproducibility.
TF_TAG=v2.16.1
BAZEL_IMAGE="gcr.io/tensorflow-sigs/build@sha256:REPLACE_WITH_VERIFIED_RELEASE_DIGEST"
: "${ARM64_SYSROOT:?set ARM64_SYSROOT to Raspberry Pi OS 64-bit sysroot}"
docker run --rm --platform linux/arm64 -v "$PWD:/work" -v "$ARM64_SYSROOT:/sysroot:ro" "$BAZEL_IMAGE" \
  bash -lc "git clone --depth 1 --branch $TF_TAG https://github.com/tensorflow/tensorflow /tmp/tensorflow && cd /tmp/tensorflow && TF_NEED_CUDA=0 PYTHON_BIN_PATH=/usr/bin/python3 ./configure && bazel build --config=opt --config=elinux_aarch64 //tensorflow/tools/pip_package:wheel && cp bazel-bin/tensorflow/tools/pip_package/wheel_house/*.whl /work/dist/"

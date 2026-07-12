"""Newline-delimited JSON worker. Stdout is protocol-only; logs go to stderr."""

from __future__ import annotations

import argparse
import json
import os
import platform
import sys
from pathlib import Path
from typing import Any, TextIO

from . import PROTOCOL_VERSION
from .training import activate_model, train
from .inference import InferenceEngine


def _memory_bytes() -> int | None:
    try:
        pages = os.sysconf("SC_PHYS_PAGES")
        page_size = os.sysconf("SC_PAGE_SIZE")
        return int(pages * page_size)
    except (ValueError, OSError, AttributeError):
        return None


def self_diagnostic(data_dir: Path, *, tensorflow: Any | None = None) -> dict[str, Any]:
    data_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    probe = data_dir / ".write-probe"
    probe.write_text("ok", encoding="utf-8")
    probe.unlink()
    tf = tensorflow
    if tf is None:
        import tensorflow as tf  # type: ignore[no-redef]  # optional isolated dependency

    tensor_result = float(tf.reduce_sum(tf.constant([1.0, 2.0, 3.0])).numpy())
    model = tf.keras.Sequential([tf.keras.layers.Input(shape=(2,)), tf.keras.layers.Dense(1)])
    prediction_shape = list(model(tf.constant([[1.0, 2.0]])).shape)
    return {
        "python_version": platform.python_version(),
        "tensorflow_version": tf.__version__,
        "keras_version": getattr(tf.keras, "__version__", "bundled"),
        "cpu": platform.processor() or platform.machine(),
        "memory_bytes": _memory_bytes(),
        "writable": True,
        "tensor_result": tensor_result,
        "prediction_shape": prediction_shape,
        "protocol_version": PROTOCOL_VERSION,
    }


def serve(data_dir: Path, source: TextIO = sys.stdin, sink: TextIO = sys.stdout) -> None:
    cancelled: set[str] = set()
    inference_engine: Any | None = None
    for line in source:
        try:
            request = json.loads(line)
            request_id = str(request["request_id"])
            version = request.get("protocol_version")
            kind = request.get("kind")
            if version != PROTOCOL_VERSION:
                raise ValueError(f"unsupported protocol version: {version}")
            if kind == "health_check":
                payload = self_diagnostic(data_dir)
            elif kind == "cancel":
                cancelled.add(str(request.get("payload", {}).get("target_request_id")))
                cancel_file = (
                    data_dir / f"cancel-{request.get('payload', {}).get('target_request_id')}"
                )
                cancel_file.touch()
                payload = {"cancelled": True}
            elif kind == "train":
                import tensorflow as tf

                training_payload = dict(request.get("payload", {}))
                training_payload["cancel_file"] = str(data_dir / f"cancel-{request_id}")
                payload = train(training_payload, data_dir, tf)
            elif kind == "activate_model":
                import tensorflow as tf

                payload = activate_model(dict(request.get("payload", {})), data_dir, tf)
                inference_engine = None
            elif kind == "infer":
                if inference_engine is None:
                    import tensorflow as tf

                    inference_engine = InferenceEngine(data_dir, tf)
                payload = inference_engine.infer(dict(request.get("payload", {})))
            elif kind == "shutdown":
                payload = {"shutdown": True}
            else:
                raise ValueError(f"unsupported request kind: {kind}")
            response = {
                "request_id": request_id,
                "protocol_version": PROTOCOL_VERSION,
                "kind": kind,
                "ok": True,
                "payload": payload,
                "error": None,
            }
        except Exception as error:  # worker errors must be structured and non-fatal
            request_id = str(locals().get("request", {}).get("request_id", "unknown"))
            response = {
                "request_id": request_id,
                "protocol_version": PROTOCOL_VERSION,
                "kind": locals().get("kind", "error"),
                "ok": False,
                "payload": {},
                "error": f"{type(error).__name__}: {error}",
            }
            print(response["error"], file=sys.stderr, flush=True)
        print(json.dumps(response, separators=(",", ":")), file=sink, flush=True)
        if response["ok"] and response["kind"] == "shutdown":
            return


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--data-dir", required=True, type=Path)
    args = parser.parse_args()
    serve(args.data_dir)


if __name__ == "__main__":
    main()

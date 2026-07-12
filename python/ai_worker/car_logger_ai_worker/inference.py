"""Long-lived, bounded TensorFlow inference with verified model fallback."""
from __future__ import annotations

import hashlib
import json
import time
from pathlib import Path
from typing import Any

import numpy as np


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


class InferenceEngine:
    """Caches one loaded model; inference requests never spawn a process."""
    def __init__(self, data_dir: Path, tf: Any):
        self.data_dir, self.tf = data_dir, tf
        self.model: Any | None = None
        self.metadata: dict[str, Any] = {}
        self.model_id: str | None = None

    def _candidates(self, scope: str) -> list[dict[str, Any]]:
        models = self.data_dir / "models"
        pointer = models / f"current-{scope}.json"
        result: list[dict[str, Any]] = []
        if pointer.is_file():
            result.append(json.loads(pointer.read_text(encoding="utf-8")))
        # Older generations are newest-first fallbacks. Candidate directories are excluded.
        for metadata_path in sorted(models.glob("*/metadata.json"), key=lambda p: p.stat().st_mtime, reverse=True):
            metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
            artifact = metadata_path.parent / "model.keras"
            item = {"model_id": metadata.get("model_id"), "artifact_path": str(artifact), "artifact_sha256": metadata.get("artifact_sha256")}
            if item["model_id"] and all(x.get("model_id") != item["model_id"] for x in result):
                result.append(item)
        return result

    def load(self, feature_schema: str, scope: str = "global") -> dict[str, Any]:
        errors = []
        candidates = self._candidates(scope)
        for index, candidate in enumerate(candidates):
            try:
                artifact = Path(candidate["artifact_path"])
                metadata = json.loads((artifact.parent / "metadata.json").read_text(encoding="utf-8"))
                expected = candidate.get("artifact_sha256") or metadata.get("artifact_sha256")
                if not artifact.is_file() or not expected or _sha256(artifact) != expected:
                    raise ValueError("artifact hash mismatch")
                if metadata.get("feature_schema") != feature_schema:
                    raise ValueError("feature schema incompatible")
                model = self.tf.keras.models.load_model(artifact, compile=False)
                if int(model.input_shape[1]) != 60:
                    raise ValueError("model window is not 60 seconds")
                self.model, self.metadata, self.model_id = model, metadata, str(metadata["model_id"])
                return {"model_id": self.model_id, "fallback": index > 0}
            except Exception as error:
                errors.append(f"{candidate.get('model_id')}: {error}")
        self.model = None
        raise ValueError("all model generations failed: " + "; ".join(errors))

    def infer(self, payload: dict[str, Any]) -> dict[str, Any]:
        schema = str(payload["feature_schema"])
        if self.model is None or self.model_id != payload.get("model_id"):
            self.load(schema, str(payload.get("scope", "global")))
        values = np.asarray(payload["values"], dtype=np.float32)
        masks = np.asarray(payload["masks"], dtype=np.float32)
        if values.ndim == 2:
            values, masks = values[None, ...], masks[None, ...]
        if values.shape != masks.shape or values.shape[0] != 1 or values.shape[1] != 60:
            raise ValueError("inference requires one 60-second values/masks window")
        if self.model.input_shape[-1] != values.shape[-1] or not np.all(np.isfinite(values)):
            raise ValueError("feature channels are incompatible or non-finite")
        started = time.perf_counter()
        predicted = np.asarray(self.model(values, training=False))
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        if predicted.shape != values.shape or not np.all(np.isfinite(predicted)):
            raise ValueError("model returned an invalid reconstruction")
        absolute = np.abs(predicted - values) * masks
        signal_denominator = np.maximum(np.sum(masks, axis=1)[0], 1.0)
        signal_errors = np.sum(absolute, axis=1)[0] / signal_denominator
        error = float(np.sum(absolute) / max(float(np.sum(masks)), 1.0))
        calibration = self.metadata["calibration"]
        score = calibrated_score(error, calibration)
        coverage = float(np.mean(masks))
        # Confidence is deliberately conservative: coverage and model evaluation quality
        # can only lower it, never manufacture certainty.
        model_quality = 1.0 - float(self.metadata.get("false_positive_rate", 0.0))
        confidence = max(0.0, min(1.0, coverage * model_quality))
        keys = list(payload["signal_keys"])
        contributions = []
        for key, value, signal_coverage in zip(keys, signal_errors, np.mean(masks, axis=1)[0]):
            percentile = error_percentile(float(value), calibration)
            contributions.append({"signal_name": key, "reconstruction_error": float(value), "normal_distribution": calibration,
                                  "percentile": percentile, "coverage": float(signal_coverage)})
        contributions.sort(key=lambda x: (x["percentile"], x["reconstruction_error"]), reverse=True)
        return {"reconstruction_error": error, "anomaly": error_percentile(error, calibration) / 100.0,
                "score": score if confidence >= 0.60 else None, "confidence": confidence, "coverage": coverage,
                "model_id": self.model_id, "feature_schema": schema, "driving_state": payload["driving_state"],
                "contributions": contributions[:3], "inference_ms": elapsed_ms}


def calibrated_score(error: float, c: dict[str, float]) -> float:
    median, p95, p99 = float(c["median"]), float(c["p95"]), float(c["p99"])
    maximum = max(float(c.get("max", p99)), p99 + 1e-12)
    if error <= median:
        score = 100 - 10 * max(error, 0) / max(median, 1e-12)
    elif error <= p95:
        score = 90 - 20 * (error - median) / (p95 - median)
    elif error <= p99:
        score = 70 - 30 * (error - p95) / (p99 - p95)
    else:
        score = 40 * max(0.0, 1 - (error - p99) / (maximum - p99))
    return float(min(100.0, max(0.0, score)))


def error_percentile(error: float, c: dict[str, float]) -> float:
    points = [(0.0, 0.0), (float(c["median"]), 50.0), (float(c["p95"]), 95.0),
              (float(c["p99"]), 99.0), (float(c.get("max", c["p99"])), 100.0)]
    for (x0, p0), (x1, p1) in zip(points, points[1:]):
        if error <= x1:
            return p1 if x1 <= x0 else p0 + (p1-p0) * (max(error, x0)-x0)/(x1-x0)
    return 100.0

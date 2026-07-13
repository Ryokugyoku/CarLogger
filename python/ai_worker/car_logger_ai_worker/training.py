"""Bounded Conv1D autoencoder training and model acceptance utilities."""

from __future__ import annotations

import gc
import hashlib
import json
import math
import os
import shutil
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable

import numpy as np

MODEL_STRUCTURE_VERSION = "conv1d-ae-v1"


class TrainingCancelled(Exception):
    pass


@dataclass(frozen=True)
class ResourceProfile:
    batch_size: int
    cpu_threads: int
    memory_limit_bytes: int


def resource_profile(memory_bytes: int | None) -> ResourceProfile:
    if memory_bytes is not None and memory_bytes < 8 * 1024**3:
        return ResourceProfile(16, 2, int(1.25 * 1024**3))
    cpus = os.cpu_count() or 2
    return ResourceProfile(64, max(1, cpus // 2), 2 * 1024**3)


def chronological_split(session_ids: list[str]) -> dict[str, list[str]]:
    """Split already-time-ordered sessions without allowing session leakage."""
    unique = list(dict.fromkeys(session_ids))
    if len(unique) < 10:
        raise ValueError("at least 10 sessions are required")
    test_count = max(3, math.ceil(len(unique) * 0.15))
    remaining = len(unique) - test_count
    validation_count = max(1, math.ceil(len(unique) * 0.15))
    calibration_count = max(1, math.ceil(len(unique) * 0.15))
    if remaining - validation_count - calibration_count < 1:
        raise ValueError("insufficient sessions for leak-free split")
    train_end = remaining - validation_count - calibration_count
    validation_end = train_end + validation_count
    return {
        "train": unique[:train_end],
        "validation": unique[train_end:validation_end],
        "calibration": unique[validation_end:remaining],
        "test": unique[remaining:],
    }


def calibrate(errors: np.ndarray) -> dict[str, float]:
    values = np.asarray(errors, dtype=np.float64)
    if values.size == 0 or not np.all(np.isfinite(values)):
        raise ValueError("calibration errors must be finite and non-empty")
    median, p95, p99 = np.percentile(values, [50, 95, 99])
    # Keep interpolation denominators positive for constant distributions.
    epsilon = max(abs(float(p99)) * 1e-9, 1e-12)
    tail_width = max(float(p99 - p95), epsilon)
    return {
        "median": float(median),
        "p95": float(max(p95, median + epsilon)),
        "p99": float(max(p99, p95 + epsilon)),
        # A single historical outlier must not stretch every future low score.
        "max": float(p99 + 4.0 * tail_width),
    }


def calibrated_score(error: float, calibration: dict[str, float]) -> float:
    if not math.isfinite(error) or any(not math.isfinite(v) for v in calibration.values()):
        return 0.0
    median, p95, p99 = calibration["median"], calibration["p95"], calibration["p99"]
    maximum = max(calibration.get("max", p99), p99 + 1e-12)
    if error <= median:
        score = 100.0 - 10.0 * max(error, 0.0) / max(median, 1e-12)
    elif error <= p95:
        score = 90.0 - 20.0 * (error - median) / (p95 - median)
    elif error <= p99:
        score = 70.0 - 30.0 * (error - p95) / (p99 - p95)
    else:
        score = 40.0 * max(0.0, 1.0 - (error - p99) / (maximum - p99))
    return float(min(100.0, max(0.0, score)))


def _masked_huber(tf: Any) -> Callable[[Any, Any], Any]:
    # y_true contains values followed by masks on the channel axis.
    def loss(y_true: Any, y_pred: Any) -> Any:
        channels = tf.shape(y_pred)[-1]
        values, mask = y_true[..., :channels], y_true[..., channels:]
        error = values - y_pred
        absolute = tf.abs(error)
        huber = tf.where(absolute <= 1.0, 0.5 * tf.square(error), absolute - 0.5)
        return tf.reduce_sum(huber * mask) / tf.maximum(tf.reduce_sum(mask), 1.0)

    return loss


def build_model(tf: Any, shape: tuple[int, int]) -> Any:
    inputs = tf.keras.Input(shape=shape)
    x = tf.keras.layers.Conv1D(32, 5, padding="same", activation="relu")(inputs)
    x = tf.keras.layers.Conv1D(16, 3, strides=2, padding="same", activation="relu")(x)
    x = tf.keras.layers.Conv1D(16, 3, padding="same", activation="relu")(x)
    latent = tf.keras.layers.GlobalAveragePooling1D(name="latent_16")(x)
    x = tf.keras.layers.RepeatVector((shape[0] + 1) // 2)(latent)
    x = tf.keras.layers.Conv1D(16, 3, padding="same", activation="relu")(x)
    x = tf.keras.layers.UpSampling1D(2)(x)
    x = tf.keras.layers.Conv1D(32, 3, padding="same", activation="relu")(x)
    outputs = tf.keras.layers.Conv1D(shape[1], 3, padding="same")(x)
    outputs = outputs[:, : shape[0], :]
    return tf.keras.Model(inputs, outputs, name=MODEL_STRUCTURE_VERSION)


def _errors(model: Any, values: np.ndarray, masks: np.ndarray, batch: int) -> np.ndarray:
    predicted = np.asarray(model.predict(values, batch_size=batch, verbose=0))
    if not np.all(np.isfinite(predicted)):
        raise ValueError("model produced NaN or Infinity")
    numerator = np.sum(np.abs(predicted - values) * masks, axis=(1, 2))
    denominator = np.maximum(np.sum(masks, axis=(1, 2)), 1.0)
    result = numerator / denominator
    if not np.all(np.isfinite(result)):
        raise ValueError("reconstruction error is not finite")
    return result


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def activate_model(payload: dict[str, Any], data_dir: Path, tf: Any) -> dict[str, Any]:
    """Verify before atomically changing the per-scope current-model pointer."""
    artifact = Path(payload["artifact_path"])
    expected_hash = str(payload["artifact_sha256"])
    if not artifact.is_file() or _sha256(artifact) != expected_hash:
        raise ValueError("model artifact is missing or hash does not match")
    metadata_path = artifact.parent / "metadata.json"
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    if metadata.get("feature_schema") != payload["feature_schema"]:
        raise ValueError("feature schema is incompatible")
    model = tf.keras.models.load_model(artifact, compile=False)
    shape = model.input_shape
    probe = np.zeros((1, int(shape[1]), int(shape[2])), dtype=np.float32)
    output = np.asarray(model.predict(probe, verbose=0))
    if output.shape != probe.shape or not np.all(np.isfinite(output)):
        raise ValueError("model load/probe failed")
    scope = str(payload.get("scope", "global"))
    pointer_dir = data_dir / "models"
    pointer_dir.mkdir(parents=True, exist_ok=True)
    pointer = pointer_dir / f"current-{scope}.json"
    temporary = pointer.with_suffix(f".{uuid.uuid4().hex}.tmp")
    temporary.write_text(
        json.dumps(
            {
                "model_id": metadata["model_id"],
                "artifact_path": str(artifact),
                "artifact_sha256": expected_hash,
            }
        ),
        encoding="utf-8",
    )
    os.replace(temporary, pointer)
    return {"activated": True, "model_id": metadata["model_id"], "scope": scope}


def train(payload: dict[str, Any], data_dir: Path, tf: Any) -> dict[str, Any]:
    started = time.monotonic()
    limit_seconds = min(float(payload.get("max_seconds", 1800)), 1800.0)
    values = np.asarray(payload["values"], dtype=np.float32)
    masks = np.asarray(payload["masks"], dtype=np.float32)
    session_ids = [str(value) for value in payload["session_ids"]]
    if values.ndim != 3 or values.shape != masks.shape or len(values) != len(session_ids):
        raise ValueError("values, masks and session_ids must describe identical windows")
    if values.shape[1] != 60 or not np.all(np.isfinite(values)) or not np.all(np.isfinite(masks)):
        raise ValueError("training input must be finite 60-second windows")
    split = chronological_split(session_ids)
    indices = {
        name: np.array([i for i, sid in enumerate(session_ids) if sid in ids])
        for name, ids in split.items()
    }
    profile = resource_profile(payload.get("memory_bytes") or _physical_memory())
    tf.config.threading.set_intra_op_parallelism_threads(profile.cpu_threads)
    tf.config.threading.set_inter_op_parallelism_threads(profile.cpu_threads)
    model = build_model(tf, (values.shape[1], values.shape[2]))
    model.compile(optimizer=tf.keras.optimizers.Adam(), loss=_masked_huber(tf))
    targets = np.concatenate([values, masks], axis=-1)

    class StopWhenRequested(tf.keras.callbacks.Callback):
        def on_epoch_end(self, epoch: int, logs: Any = None) -> None:
            cancel_file = payload.get("cancel_file")
            if time.monotonic() - started >= limit_seconds or (
                cancel_file and Path(cancel_file).exists()
            ):
                self.model.stop_training = True
                self.cancelled = True

    stopper = StopWhenRequested()
    stopper.cancelled = False
    early = tf.keras.callbacks.EarlyStopping(
        monitor="val_loss", patience=8, restore_best_weights=True, mode="min"
    )
    batch = min(profile.batch_size, max(1, len(indices["train"])))
    while True:
        try:
            history = model.fit(
                values[indices["train"]],
                targets[indices["train"]],
                validation_data=(values[indices["validation"]], targets[indices["validation"]]),
                epochs=100,
                batch_size=batch,
                callbacks=[early, stopper],
                verbose=0,
                shuffle=False,
            )
            break
        except tf.errors.ResourceExhaustedError:
            if batch == 1:
                raise MemoryError("training exhausted memory at batch size 1")
            batch = max(1, batch // 2)
            tf.keras.backend.clear_session()
            gc.collect()
            model = build_model(tf, (values.shape[1], values.shape[2]))
            model.compile(optimizer=tf.keras.optimizers.Adam(), loss=_masked_huber(tf))
    if stopper.cancelled:
        raise TrainingCancelled("training cancelled or exceeded 30 minutes")

    calibration_errors = _errors(
        model, values[indices["calibration"]], masks[indices["calibration"]], batch
    )
    calibration = calibrate(calibration_errors)
    test_errors = _errors(model, values[indices["test"]], masks[indices["test"]], batch)
    false_positive_rate = float(np.mean(test_errors > calibration["p95"]))
    synthetic = values[indices["test"]].copy()
    synthetic[:, 20:40, :] += 3.0
    synthetic_errors = _errors(model, synthetic, masks[indices["test"]], batch)
    inference_start = time.perf_counter()
    _errors(model, values[indices["test"]][:1], masks[indices["test"]][:1], 1)
    inference_ms = (time.perf_counter() - inference_start) * 1000
    current_loss = payload.get("current_validation_loss")
    validation_loss = float(min(history.history["val_loss"]))
    reasons = []
    if false_positive_rate > 0.05:
        reasons.append("false_positive_rate_above_5_percent")
    if current_loss is not None and validation_loss > float(current_loss) * 1.25:
        reasons.append("validation_loss_regressed")
    if float(np.median(synthetic_errors)) <= float(np.median(test_errors)):
        reasons.append("synthetic_change_not_detected")
    if inference_ms > 5000:
        reasons.append("inference_slower_than_5_seconds")

    generation = str(payload.get("model_id") or uuid.uuid4())
    candidate_dir = data_dir / "models" / f".{generation}.candidate"
    final_dir = data_dir / "models" / generation
    shutil.rmtree(candidate_dir, ignore_errors=True)
    candidate_dir.mkdir(parents=True, mode=0o700)
    artifact = candidate_dir / "model.keras"
    model.save(artifact)
    metadata = {
        "model_id": generation,
        "scope": payload.get("scope", "global"),
        "tensorflow_version": tf.__version__,
        "keras_version": getattr(tf.keras, "__version__", "bundled"),
        "model_structure_version": MODEL_STRUCTURE_VERSION,
        "feature_schema": payload["feature_schema"],
        "normalization": payload.get("normalization", {}),
        "training_data_range": payload.get("data_range"),
        "counts": {key: int(len(indices[key])) for key in indices},
        "losses": {
            "training": float(min(history.history["loss"])),
            "validation": validation_loss,
            "evaluation": float(np.mean(test_errors)),
        },
        "false_positive_rate": false_positive_rate,
        "calibration": calibration,
        "inference_ms": inference_ms,
        "epochs": len(history.history["loss"]),
        "batch_size": batch,
        "state": "candidate" if reasons else "accepted",
        "decision_reasons": reasons,
    }
    metadata["artifact_sha256"] = _sha256(artifact)
    (candidate_dir / "metadata.json").write_text(
        json.dumps(metadata, sort_keys=True), encoding="utf-8"
    )
    if not reasons:
        shutil.rmtree(final_dir, ignore_errors=True)
        os.replace(candidate_dir, final_dir)
        metadata["artifact_path"] = str(final_dir / "model.keras")
    else:
        metadata["artifact_path"] = str(artifact)
    return metadata


def _physical_memory() -> int | None:
    try:
        return int(os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE"))
    except (ValueError, OSError, AttributeError):
        return None

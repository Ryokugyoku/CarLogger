from types import SimpleNamespace

import numpy as np

from car_logger_ai_worker.inference import InferenceEngine, calibrated_score, error_percentile


CALIBRATION = {"median": 1.0, "p95": 2.0, "p99": 3.0, "max": 4.0}


def test_calibration_boundaries():
    assert calibrated_score(1.0, CALIBRATION) == 90.0
    assert calibrated_score(2.0, CALIBRATION) == 70.0
    assert calibrated_score(3.0, CALIBRATION) == 40.0
    assert calibrated_score(4.0, CALIBRATION) == 0.0


def test_percentile_boundaries():
    assert error_percentile(1.0, CALIBRATION) == 50.0
    assert error_percentile(2.0, CALIBRATION) == 95.0
    assert error_percentile(3.0, CALIBRATION) == 99.0


def test_inference_contributions_have_the_explanation_contract(tmp_path):
    class Model:
        input_shape = (None, 60, 4)

        def __call__(self, values, training=False):
            return np.zeros_like(values)

    engine = InferenceEngine(tmp_path, SimpleNamespace())
    engine.model = Model()
    engine.model_id = "model-1"
    engine.metadata = {"calibration": CALIBRATION, "false_positive_rate": 0.01}
    result = engine.infer(
        {
            "feature_schema": "schema-1",
            "model_id": "model-1",
            "values": np.ones((60, 4)).tolist(),
            "masks": np.ones((60, 4)).tolist(),
            "signal_keys": ["rpm", "speed", "load", "coolant"],
            "driving_state": "steady_cruise",
            "window_start": "2026-01-01T00:00:00Z",
        }
    )
    contribution = result["contributions"][0]
    assert contribution["rank"] == 1
    assert contribution["normal_median"] == CALIBRATION["median"]
    assert contribution["driving_state"] == "steady_cruise"
    assert contribution["window_start"] == "2026-01-01T00:00:00Z"

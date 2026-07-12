import io
import json
from types import SimpleNamespace

import pytest

from car_logger_ai_worker.worker import self_diagnostic, serve
from car_logger_ai_worker.training import (
    calibrate,
    calibrated_score,
    chronological_split,
    resource_profile,
)


class Value:
    def __init__(self, value):
        self.value = value

    def numpy(self):
        return self.value


class FakeModel:
    def __call__(self, value):
        return SimpleNamespace(shape=(1, 1))


class FakeTensorFlow:
    __version__ = "test"
    keras = SimpleNamespace(
        __version__="test",
        Sequential=lambda layers: FakeModel(),
        layers=SimpleNamespace(Input=lambda **kw: None, Dense=lambda n: None),
    )
    constant = staticmethod(lambda value: value)
    reduce_sum = staticmethod(lambda value: Value(sum(value)))


def test_self_diagnostic(tmp_path):
    result = self_diagnostic(tmp_path, tensorflow=FakeTensorFlow())
    assert result["tensor_result"] == 6.0
    assert result["prediction_shape"] == [1, 1]
    assert result["writable"] is True


def test_protocol_mismatch_is_structured(tmp_path):
    source = io.StringIO(
        json.dumps(
            {"request_id": "r", "protocol_version": 9, "kind": "health_check", "payload": {}}
        )
        + "\n"
    )
    sink = io.StringIO()
    serve(tmp_path, source, sink)
    response = json.loads(sink.getvalue())
    assert response["ok"] is False
    assert "unsupported protocol" in response["error"]


def test_tensorflow_load_failure_is_ai_only(tmp_path, monkeypatch):
    original_import = __import__

    def fail(name, *args, **kwargs):
        if name == "tensorflow":
            raise ImportError("tensorflow unavailable")
        return original_import(name, *args, **kwargs)

    monkeypatch.setattr("builtins.__import__", fail)
    with pytest.raises(ImportError):
        self_diagnostic(tmp_path)


def test_chronological_split_has_no_session_leak_and_three_test_sessions():
    sessions = [f"s{i}" for i in range(12)]
    split = chronological_split(sessions)
    assert len(split["test"]) >= 3
    assert split["train"] + split["validation"] + split["test"] == sessions
    assert set(split["train"]).isdisjoint(split["validation"])
    assert set(split["train"]).isdisjoint(split["test"])
    assert set(split["validation"]).isdisjoint(split["test"])


def test_split_waits_when_sessions_are_insufficient():
    with pytest.raises(ValueError):
        chronological_split([f"s{i}" for i in range(9)])


def test_calibration_boundaries_are_finite_and_bounded():
    calibration = calibrate(__import__("numpy").array(range(1, 101), dtype=float))
    assert 90 <= calibrated_score(calibration["median"], calibration) <= 100
    assert 70 <= calibrated_score(calibration["p95"], calibration) <= 90
    assert 40 <= calibrated_score(calibration["p99"], calibration) <= 70
    assert calibrated_score(float("nan"), calibration) == 0
    assert 0 <= calibrated_score(1e100, calibration) <= 100


def test_pi_4gb_profile_is_bounded():
    profile = resource_profile(4 * 1024**3)
    assert profile.batch_size == 16
    assert profile.cpu_threads == 2
    assert profile.memory_limit_bytes == int(1.25 * 1024**3)

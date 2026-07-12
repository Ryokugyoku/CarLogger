import io
import json
from types import SimpleNamespace

import pytest

from car_logger_ai_worker.worker import self_diagnostic, serve


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

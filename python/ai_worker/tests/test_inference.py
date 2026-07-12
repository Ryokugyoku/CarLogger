from car_logger_ai_worker.inference import calibrated_score, error_percentile


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

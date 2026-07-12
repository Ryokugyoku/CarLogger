# AI distribution, update, privacy, and acceptance

## Release artifacts

Build with `TARGET_TRIPLE=<triple> distribution/build-runtime.sh`. Each artifact bundles
the application, an isolated Python runtime, TensorFlow/Keras, and the worker. The fixed
versions live in `distribution/versions.env`; `BUILD-MANIFEST.txt`, dependency lock files,
third-party licenses, `PYTHON-DEPENDENCIES.txt`, and `SHA256SUMS` record the target OS/CPU
and provenance. Raspberry Pi full TensorFlow is built in
release CI with `build-tensorflow-arm64.sh`, never on the user's device. Pi 5 with 4 GB is
the minimum and 8 GB or more is recommended.

## Compatibility and recovery

App, worker protocol, model structure, and feature schema have independent versions.
Before an update, retain the current and two prior model generations. After updating,
run the worker self-diagnostic and test inference. A schema/model mismatch leaves the old
model untouched, schedules automatic retraining, and uses statistical scoring until the
candidate passes evaluation. DuckDB migrations use `IF NOT EXISTS`, `ADD COLUMN IF NOT
EXISTS`, and singleton upserts, so rerunning them is idempotent.

If update validation fails: stop only the AI worker, restore the previous application and
runtime artifact after verifying `SHA256SUMS`, retain the database/model directory, launch
CarLogger, and run Self diagnostic. Normal OBD2 logging and statistical scoring remain
enabled throughout. Never manually point `current.json` at an unverified model.

## Privacy and trust boundary

Training and inference are local-only and have no automatic upload path. Feature building
must exclude VIN and location fields; export is initiated only by the user. AI storage is
created with owner-only permissions. Model activation verifies SHA-256, schema and load/test
inference; arbitrary external models are never automatically loaded.

## Acceptance record (2026-07-12)

Automated source tests cover scoring, training cancellation, candidate acceptance/rejection,
atomic adoption, rollback storage rules, worker crash/timeout, corrupt/incompatible model
handling, privacy-oriented feature schema, and idempotent storage initialization. The local
macOS environment is used only for source-level tests in this change.

The following remain **unverified**, not inferred successful: Windows x86-64 runtime and
installer; Apple Silicon package; macOS Intel package and real hardware; Linux x86-64
package; Raspberry Pi 5 4 GB memory behavior; GTK tooltip/focus and narrow-window visual QA;
crash recovery and update migration on every packaged OS. The release matrix makes build
and automated tests mandatory, including macOS Intel, but it does not turn an absent real
device run into a pass. Record device/OS, artifact SHA-256, test case, result, logs, memory
peak, and reviewer for each manual acceptance run.

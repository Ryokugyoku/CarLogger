# Third-party license inventory

Release artifacts must include the license texts emitted by `cargo-about generate` and
`pip-licenses --format=markdown --with-license-file`. TensorFlow/Keras are Apache-2.0,
Python is PSF-2.0, GTK is LGPL-2.1-or-later, and Rust dependencies retain the licenses
recorded in `Cargo.lock`. CI fails release publication when this generated inventory or
`SHA256SUMS` is absent.

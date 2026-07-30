check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

ci: check test

# Two-way ASC MHL conformance against the Python reference implementation.
# `uv venv` doesn't install a `pip` binary — install via `uv pip` targeting
# the venv's interpreter instead. Paths must be absolute: `cargo test` runs
# test binaries with the package directory (crates/ingest) as the working
# directory, not the workspace root the venv was created in.
conformance:
    uv venv --allow-existing .ascmhl-venv
    uv pip install --quiet --python .ascmhl-venv/bin/python ascmhl==1.2
    ASCMHL_BIN="{{justfile_directory()}}/.ascmhl-venv/bin/ascmhl" \
        ASCMHL_DEBUG_BIN="{{justfile_directory()}}/.ascmhl-venv/bin/ascmhl-debug" \
        cargo test -p majestical-ingest --test conformance -- --ignored

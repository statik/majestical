//! Shared test-only fixtures for the ingest crate's integration tests:
//! `engine.rs` (a normal `#[test]`-harnessed suite) and `acceptance.rs` (a
//! `harness = false` cucumber binary). Kept entirely panic-free — no
//! unwrap/expect/panic — so it compiles cleanly under both without relying
//! on clippy's test-code exemptions, which key off a literal `#[cfg(test)]`
//! on each item rather than the ambient `cfg(test)` a `--test` build sets.
//! `mod common;` in each file pulls this in; cargo does not treat a
//! subdirectory under `tests/` as its own test binary, so this file is never
//! itself discovered as a separate target.
use majestical_ingest::engine::{
    CancelFlag, ProgressEvent, RealSinks, RunControl, Sink, SinkFactory,
};
use std::io::Write;
use std::path::Path;

/// A `RunControl` that discards every progress event and never cancels —
/// for the tests whose subject is the copy result, not the event stream.
pub fn silent_control() -> RunControl<'static> {
    static PROGRESS: fn(ProgressEvent) = |_event| {};
    static NEVER: CancelFlag = CancelFlag::new(false);
    RunControl {
        progress: &PROGRESS,
        cancel: &NEVER,
    }
}

/// A `SinkFactory` that flips the first byte it writes for any path whose
/// name contains `target`, corrupting the destination between write and
/// read-back — exactly the failure read-back verification exists to catch.
pub struct CorruptingSinks {
    pub target: String,
}

pub struct CorruptingSink {
    inner: Box<dyn Sink>,
    corrupt: bool,
    done: bool,
}

impl Write for CorruptingSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.corrupt && !self.done && !buf.is_empty() {
            self.done = true;
            let mut flipped = buf.to_vec();
            flipped[0] ^= 0xFF;
            return self.inner.write(&flipped);
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Sink for CorruptingSink {
    fn finish(&mut self) -> std::io::Result<()> {
        self.inner.finish()
    }
}

impl SinkFactory for CorruptingSinks {
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn Sink>> {
        let corrupt = path.to_string_lossy().contains(&self.target);
        Ok(Box::new(CorruptingSink {
            inner: RealSinks.open(path)?,
            corrupt,
            done: false,
        }))
    }
}

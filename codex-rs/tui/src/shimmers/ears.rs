//! Ears: what the speakers play, captured through `PipeWire` for a shimmer that listens.
//!
//! `pw-record` records the default sink's monitor as libshimmer hears sound, mono 32-bit floats at
//! `SAMPLE_RATE`, and follows the default sink as it changes. The capture is passive: it never
//! wakes a suspended sink or holds one awake. A reader thread moves the samples into a buffer that
//! the frame loop drains into the shimmer. Without `PipeWire` nothing listens, and codex-tui
//! withholds the shimmers that would.

use std::io::Read as _;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::PoisonError;

use libshimmer::SAMPLE_RATE;

/// Samples the buffer keeps while the frame loop does not drain it: a quarter second.
const KEEP: usize = 12_000;
/// Samples the reader moves at once, about 5 ms of sound.
const CHUNK: usize = 256;

/// A running capture; dropping it stops the recorder.
pub(super) struct Ears {
    recorder: Child,
    heard: Arc<Mutex<Vec<f32>>>,
}

impl Ears {
    /// Whether this session can hear: `PipeWire`'s socket is up and `pw-record` is installed.
    pub(super) fn can_hear() -> bool {
        static CAN: LazyLock<bool> = LazyLock::new(|| {
            std::env::var_os("XDG_RUNTIME_DIR")
                .is_some_and(|dir| Path::new(&dir).join("pipewire-0").exists())
                && std::env::var_os("PATH").is_some_and(|path| {
                    std::env::split_paths(&path).any(|dir| dir.join("pw-record").is_file())
                })
        });
        *CAN
    }

    /// Starts listening, or nothing when the recorder cannot start.
    pub(super) fn open() -> Option<Self> {
        let rate = SAMPLE_RATE.to_string();
        let recorder = Command::new("pw-record")
            .args([
                "--raw",
                "--format",
                "f32",
                "--channels",
                "1",
                "--rate",
                rate.as_str(),
            ])
            .args([
                "--properties",
                "{ stream.capture.sink = true node.passive = true node.name = codex-tui-ears }",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        // From here on, giving up drops the ears, which stops the recorder.
        let mut ears = Self {
            recorder,
            heard: Arc::default(),
        };
        let mut sound = ears.recorder.stdout.take()?;
        let heard = Arc::clone(&ears.heard);
        std::thread::Builder::new()
            .name("shimmer-ears".to_owned())
            .spawn(move || {
                let mut bytes = [0; 4 * CHUNK];
                while sound.read_exact(&mut bytes).is_ok() {
                    let (samples, _) = bytes.as_chunks::<4>();
                    let mut heard = heard.lock().unwrap_or_else(PoisonError::into_inner);
                    heard.extend(samples.iter().copied().map(f32::from_le_bytes));
                    let stale = heard.len().saturating_sub(KEEP);
                    heard.drain(..stale);
                }
            })
            .ok()?;
        Some(ears)
    }

    /// What the speakers played since the last call.
    pub(super) fn drain(&self) -> Vec<f32> {
        std::mem::take(&mut *self.heard.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl Drop for Ears {
    fn drop(&mut self) {
        // The reader thread ends with the recorder's output.
        let _ = self.recorder.kill();
        let _ = self.recorder.wait();
    }
}

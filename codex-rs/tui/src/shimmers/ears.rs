//! Ears: what the speakers play, captured through `PipeWire` for a shimmer that listens.
//!
//! `pw-record` records the monitor of the sink that is playing, as libshimmer hears sound: mono
//! 32-bit floats at `SAMPLE_RATE`. `pactl subscribe` reports every sink change; when another sink
//! starts playing, the ears move to it, and while none plays they stay where they are. Until a sink
//! plays they hear the default sink. The capture is passive: it never wakes a suspended sink or
//! holds one awake. A reader thread moves the samples into a buffer that the frame loop drains
//! into the shimmer. Without `PipeWire`, `pw-record` and `pactl` nothing listens, and codex-tui
//! withholds the shimmers that would.

use std::io::BufRead as _;
use std::io::BufReader;
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

type Heard = Arc<Mutex<Vec<f32>>>;

/// A running capture; dropping it stops the recorder.
pub(super) struct Ears {
    /// `pactl subscribe`, whose sink events steer the recorder. Killing it ends the steering
    /// thread, which stops the recorder.
    sinks: Child,
    heard: Heard,
}

impl Ears {
    /// Whether this session can hear: `PipeWire`'s socket is up, and `pw-record` and `pactl` are
    /// installed.
    pub(super) fn can_hear() -> bool {
        static CAN: LazyLock<bool> = LazyLock::new(|| {
            std::env::var_os("XDG_RUNTIME_DIR")
                .is_some_and(|dir| Path::new(&dir).join("pipewire-0").exists())
                && std::env::var_os("PATH").is_some_and(|path| {
                    ["pw-record", "pactl"].iter().all(|tool| {
                        std::env::split_paths(&path).any(|dir| dir.join(tool).is_file())
                    })
                })
        });
        *CAN
    }

    /// Starts listening, or nothing when the recorder cannot start.
    pub(super) fn open() -> Option<Self> {
        let mut sinks = Command::new("pactl")
            .arg("subscribe")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let events = sinks.stdout.take();
        // From here on, giving up drops the ears, which stops the subscription and the recorder.
        let ears = Self {
            sinks,
            heard: Heard::default(),
        };
        let heard = Arc::clone(&ears.heard);
        let mut recorder = Recorder::start(playing_sink(/*current*/ None), &heard)?;
        let events = events?;
        std::thread::Builder::new()
            .name("shimmer-ears-sinks".to_owned())
            .spawn(move || {
                for event in BufReader::new(events).lines().map_while(Result::ok) {
                    if !event.contains(" on sink #") {
                        continue;
                    }
                    if let Some(sink) = playing_sink(recorder.sink.as_deref())
                        && recorder.sink.as_deref() != Some(sink.as_str())
                        && let Some(moved) = Recorder::start(Some(sink), &heard)
                    {
                        recorder = moved;
                    }
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
        // The steering thread ends with the subscription's output and stops the recorder.
        let _ = self.sinks.kill();
        let _ = self.sinks.wait();
    }
}

/// One `pw-record` on one sink's monitor; dropping it stops the recording.
struct Recorder {
    child: Child,
    /// The sink recorded, or `None` for whichever sink is the default.
    sink: Option<String>,
}

impl Recorder {
    fn start(sink: Option<String>, heard: &Heard) -> Option<Self> {
        let rate = SAMPLE_RATE.to_string();
        let mut command = Command::new("pw-record");
        command.args([
            "--raw",
            "--format",
            "f32",
            "--channels",
            "1",
            "--rate",
            rate.as_str(),
        ]);
        if let Some(sink) = &sink {
            command.args(["--target", sink.as_str()]);
        }
        let child = command
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
        // From here on, giving up drops the recorder, which stops `pw-record`.
        let mut recorder = Self { child, sink };
        let mut sound = recorder.child.stdout.take()?;
        let heard = Arc::clone(heard);
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
        Some(recorder)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // The reader thread ends with the recorder's output.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The sink playing now: `current` while it still plays, otherwise the first that does.
fn playing_sink(current: Option<&str>) -> Option<String> {
    let listing = Command::new("pactl")
        .args(["list", "short", "sinks"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let listing = String::from_utf8_lossy(&listing.stdout);
    let playing: Vec<&str> = listing
        .lines()
        .filter_map(
            |line| match line.split('\t').collect::<Vec<_>>().as_slice() {
                [_, sink, _, _, "RUNNING", ..] => Some(*sink),
                _ => None,
            },
        )
        .collect();
    playing
        .iter()
        .find(|sink| Some(**sink) == current)
        .or(playing.first())
        .map(|sink| (*sink).to_owned())
}

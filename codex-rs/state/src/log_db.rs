//! TELOS(no-feedback-upload): no log database writer exists. Upstream threads an optional
//! `LogDbLayer` through many files; it is uninhabited here, so every `Option<LogDbLayer>` is
//! `None` and every use of one is statically dead. Diagnostics live in `codex-flight-recorder`.

/// Uninhabited: a log database writer cannot be constructed.
#[derive(Clone, Copy, Debug)]
pub enum LogDbLayer {}

impl LogDbLayer {
    pub async fn flush(&self) {
        match *self {}
    }

    pub fn has_write_failure(&self) -> bool {
        match *self {}
    }
}

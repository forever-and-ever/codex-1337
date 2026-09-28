//! Optional local desktop observation. Failure to observe must leave painting usable.

use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DesktopAttention {
    pub(crate) idle: bool,
    /// `None` never proves that a window is hidden.
    pub(crate) visible: Option<bool>,
}

pub(crate) fn start() -> watch::Receiver<DesktopAttention> {
    let (_, receiver) = watch::channel(DesktopAttention::default());
    receiver
}

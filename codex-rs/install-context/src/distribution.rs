//! The codex-1337 distribution an executable was installed from.
//!
//! `scripts/install-local` publishes immutable bundles
//! `<distributions>/<identity>/{bin/,distribution}` and links the installed
//! release of each channel as `<distributions>/current` or
//! `<distributions>/staging`. The `distribution` manifest holds `key=value`
//! lines; a trial build's carries `channel=staging`.
//!
//! Staging runs its own daemon beside production's, so a broken trial build
//! cannot take other conversations down. Both share the Codex home:
//! configuration, credentials, sessions and the SQLite state. Only the daemon's
//! runtime files differ. The channel is a property of the installed bundle, not
//! of the environment, so commands that agents run never inherit it.

use std::io;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;

const MANIFEST_FILENAME: &str = "distribution";
const MANIFEST_LIMIT: u64 = 4096;

static RUNNING: LazyLock<Option<Distribution>> = LazyLock::new(|| {
    match std::env::current_exe().and_then(|executable| Distribution::of(&executable)) {
        Ok(distribution) => distribution,
        Err(error) => {
            tracing::warn!(%error, "cannot read the running Codex distribution");
            None
        }
    }
});

/// The installed release a process belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Channel {
    Production,
    /// A trial build serving its own daemon beside production.
    Staging,
}

impl Channel {
    /// The running executable's channel; uninstalled builds are production.
    pub fn current() -> Self {
        Distribution::running().map_or(Self::Production, Distribution::channel)
    }

    /// Root of this channel's daemon runtime files under `codex_home`.
    pub fn daemon_home(self, codex_home: &Path) -> PathBuf {
        match self {
            Self::Production => codex_home.to_path_buf(),
            Self::Staging => codex_home.join("staging"),
        }
    }

    fn link(self) -> &'static str {
        match self {
            Self::Production => "current",
            Self::Staging => "staging",
        }
    }
}

/// An installed bundle, as its manifest describes it.
#[derive(Debug, Eq, PartialEq)]
pub struct Distribution {
    distributions: PathBuf,
    channel: Channel,
    tui_revision: Option<u64>,
}

impl Distribution {
    /// The running executable's distribution, absent for an uninstalled build.
    pub fn running() -> Option<&'static Self> {
        RUNNING.as_ref()
    }

    /// The distribution holding `executable`, absent for an uninstalled build.
    pub fn of(executable: &Path) -> io::Result<Option<Self>> {
        let Some(bundle) = executable
            .parent()
            .filter(|bin| bin.file_name().is_some_and(|name| name == "bin"))
            .and_then(Path::parent)
        else {
            return Ok(None);
        };
        let Some(distributions) = bundle.parent().filter(|parent| {
            parent
                .file_name()
                .is_some_and(|name| name == "distributions")
        }) else {
            return Ok(None);
        };
        let mut manifest = String::new();
        match std::fs::File::open(bundle.join(MANIFEST_FILENAME)) {
            Ok(file) => file.take(MANIFEST_LIMIT).read_to_string(&mut manifest)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut distribution = Self {
            distributions: distributions.to_path_buf(),
            channel: Channel::Production,
            tui_revision: None,
        };
        for field in manifest.lines().filter_map(|line| line.split_once('=')) {
            match field {
                ("channel", "staging") => distribution.channel = Channel::Staging,
                ("channel", channel) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unknown distribution channel `{channel}`"),
                    ));
                }
                ("tui_revision", revision) => {
                    distribution.tui_revision = Some(
                        revision
                            .parse()
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                    );
                }
                _ => {}
            }
        }
        Ok(Some(distribution))
    }

    pub fn channel(&self) -> Channel {
        self.channel
    }

    /// The number `scripts/tui_revision.py` assigned to this bundle's binary.
    pub fn tui_revision(&self) -> Option<u64> {
        self.tui_revision
    }

    /// The executable of this channel's latest installed release.
    pub fn entrypoint(&self) -> PathBuf {
        self.distributions
            .join(self.channel.link())
            .join("bin/codex")
    }
}

#[cfg(test)]
#[path = "distribution_tests.rs"]
mod tests;

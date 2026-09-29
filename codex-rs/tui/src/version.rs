use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

/// The current Codex CLI version as embedded at compile time.
pub const CODEX_CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The owned distribution identity shown on Codex's user-facing surfaces.
pub const CODEX_DISPLAY_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-1337");

static TUI_REVISION: LazyLock<Option<u64>> = LazyLock::new(|| {
    match std::env::current_exe().and_then(|executable| read_tui_revision(&executable)) {
        Ok(revision) => revision,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(%error, "cannot read installed TUI revision");
            None
        }
    }
});

pub(crate) fn status_line_version(server_version: Option<&str>) -> String {
    format_status_version(server_version.unwrap_or(CODEX_CLI_VERSION), *TUI_REVISION)
}

fn format_status_version(server_version: &str, revision: Option<u64>) -> String {
    let server = server_version.strip_prefix('v').unwrap_or(server_version);
    let suffix = if server == "unknown" || server.ends_with("-1337") {
        ""
    } else {
        "-1337"
    };
    let revision = revision.map_or_else(|| "dev".to_string(), |value| value.to_string());
    format!("{server}{suffix}/tui-{revision}")
}

fn read_tui_revision(executable: &Path) -> std::io::Result<Option<u64>> {
    let bundle = executable
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| std::io::Error::other("TUI executable has no bundle directory"))?;
    let mut manifest = String::new();
    std::fs::File::open(bundle.join("distribution"))?
        .take(/*limit*/ 4096)
        .read_to_string(&mut manifest)?;
    manifest
        .lines()
        .find_map(|line| line.strip_prefix("tui_revision="))
        .map(|value| {
            value
                .parse()
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
        .transpose()
}

#[cfg(test)]
#[path = "version_tests.rs"]
mod tests;

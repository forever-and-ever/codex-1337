use codex_install_context::Channel;
use codex_install_context::Distribution;

/// The current Codex CLI version as embedded at compile time.
pub const CODEX_CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The owned distribution identity shown on Codex's user-facing surfaces.
pub const CODEX_DISPLAY_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-1337");

pub(crate) fn status_line_version(server_version: Option<&str>) -> String {
    let distribution = Distribution::running();
    let version = format_status_version(
        server_version.unwrap_or(CODEX_CLI_VERSION),
        distribution.and_then(Distribution::tui_revision),
    );
    match distribution.map(Distribution::channel) {
        Some(Channel::Staging) => format!("{version} staging"),
        Some(Channel::Production) | None => version,
    }
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

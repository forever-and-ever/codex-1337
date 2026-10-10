//! A staging installation's guard rails: systemd alone runs its daemon.

use codex_core::config::find_codex_home;
use codex_install_context::Channel;

use crate::AppServerCommand;
use crate::AppServerSubcommand;
use crate::Subcommand;

/// Refuses daemon lifecycle commands in a staging installation, and prepares
/// the parent of its daemon's runtime files, which the daemon creates without
/// parents.
pub(crate) fn admit(subcommand: Option<&Subcommand>) -> anyhow::Result<()> {
    if Channel::current() == Channel::Production {
        return Ok(());
    }
    if matches!(
        subcommand,
        Some(
            Subcommand::AppServer(AppServerCommand {
                subcommand: Some(AppServerSubcommand::Daemon(_)),
                ..
            }) | Subcommand::RemoteControl(_)
        )
    ) {
        anyhow::bail!(
            "the staging daemon is systemd-managed; reinstall it with `scripts/install-local --staging` or inspect it with `systemctl --user status codex-app-server-staging`"
        );
    }
    std::fs::create_dir_all(Channel::Staging.daemon_home(find_codex_home()?.as_path()))?;
    Ok(())
}

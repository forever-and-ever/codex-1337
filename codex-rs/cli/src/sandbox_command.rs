use super::HostSandboxArgs;
use super::loader_overrides_for_profile;
use super::prepend_config_flags;
use super::sandbox_uninstall::SandboxUninstallCommand;
use clap::Parser;
use codex_arg0::Arg0DispatchPaths;
use codex_utils_cli::CliConfigOverrides;
use codex_utils_cli::ProfileV2Name;

// TELOS(explicit-command-verbs): unknown verbs never become host executables.
#[derive(Debug, Parser)]
pub(super) struct SandboxCommand {
    #[command(subcommand)]
    pub(super) subcommand: SandboxSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub(super) enum SandboxSubcommand {
    /// Run a command using the host OS sandbox backend.
    Run(HostSandboxArgs),
    /// Remove the legacy Windows sandbox's machine-wide accounts and network rules.
    Uninstall(SandboxUninstallCommand),
}

impl SandboxCommand {
    pub(super) async fn run(
        self,
        root_profile: Option<&ProfileV2Name>,
        root_overrides: CliConfigOverrides,
        arg0_paths: Arg0DispatchPaths,
    ) -> anyhow::Result<()> {
        let mut sandbox_cli = match self.subcommand {
            SandboxSubcommand::Run(host) => host,
            SandboxSubcommand::Uninstall(command) => return command.run(),
        };
        let config_profile = sandbox_cli.config_profile.as_ref().or(root_profile);
        prepend_config_flags(&mut sandbox_cli.config_overrides, root_overrides);
        #[cfg(target_os = "windows")]
        if let Some(setup_cli) = super::sandbox_setup::parse_setup_command(&sandbox_cli.command)? {
            let cli_overrides = sandbox_cli
                .config_overrides
                .parse_overrides()
                .map_err(anyhow::Error::msg)?;
            return super::sandbox_setup::run(setup_cli, config_profile.cloned(), cli_overrides).await;
        }
        let loader_overrides = loader_overrides_for_profile(config_profile)?;
        #[cfg(target_os = "macos")]
        codex_cli::run_command_under_seatbelt(
            sandbox_cli,
            arg0_paths.codex_linux_sandbox_exe,
            loader_overrides,
        )
        .await?;
        #[cfg(target_os = "linux")]
        codex_cli::run_command_under_landlock(
            sandbox_cli,
            arg0_paths.codex_linux_sandbox_exe,
            loader_overrides,
        )
        .await?;
        #[cfg(target_os = "windows")]
        codex_cli::run_command_under_windows_sandbox(
            sandbox_cli,
            arg0_paths.codex_linux_sandbox_exe,
            loader_overrides,
        )
        .await?;
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            let _ = loader_overrides;
            anyhow::bail!("`codex sandbox` is not supported on this operating system");
        }
        Ok(())
    }
}

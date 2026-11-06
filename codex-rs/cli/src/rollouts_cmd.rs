//! `codex rollouts compress`: converts every plain and single-frame rollout to the framed encoding.
//!
//! Each conversion holds its thread's writer lock: a live thread is skipped for a later run, and a
//! thread resumed during its own conversion briefly reports an active writer. Stored rollout paths
//! name the logical `.jsonl` file whatever its encoding, so no stored row changes.

use clap::Parser;
use clap::Subcommand;
use codex_core::config::ConfigBuilder;
use codex_rollout::RolloutCensus;
use codex_utils_cli::CliConfigOverrides;

#[derive(Debug, Parser)]
pub(crate) struct RolloutsCommand {
    #[command(subcommand)]
    action: RolloutsAction,
}

#[derive(Debug, Subcommand)]
enum RolloutsAction {
    /// Convert plain and single-frame rollouts to the framed encoding.
    Compress {
        /// Only count rollouts per encoding.
        #[arg(long)]
        check: bool,
    },
}

pub(crate) async fn run(
    command: RolloutsCommand,
    config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let RolloutsAction::Compress { check } = command.action;
    let overrides = config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let config = ConfigBuilder::default()
        .cli_overrides(overrides)
        .build()
        .await?;
    let codex_home = config.codex_home.to_path_buf();
    let rollouts = {
        let codex_home = codex_home.clone();
        tokio::task::spawn_blocking(move || codex_rollout::local_rollouts(codex_home.as_path()))
            .await??
    };
    let census = RolloutCensus::of(&rollouts);
    println!(
        "{} plain, {} single-frame, {} framed rollouts",
        census.plain, census.single_frame, census.framed
    );
    if check || census.unconverted() == 0 {
        return Ok(());
    }

    let tally = tokio::task::spawn_blocking(move || {
        codex_rollout::convert_rollouts(codex_home.as_path(), rollouts)
    })
    .await?;
    for (path, err) in &tally.failed {
        eprintln!("{}: {err}", path.display());
    }
    println!(
        "converted {} rollouts: {} MiB -> {} MiB",
        tally.converted,
        tally.bytes_before >> 20,
        tally.bytes_after >> 20
    );
    if tally.busy > 0 {
        println!(
            "{} rollouts have live writers; rerun once those threads are idle",
            tally.busy
        );
    }
    anyhow::ensure!(
        tally.failed.is_empty(),
        "{} rollouts failed to convert",
        tally.failed.len()
    );
    Ok(())
}

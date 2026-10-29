//! Preserve config overrides across command boundaries without changing typed child arguments.

use crate::MultitoolCli;
use clap::Parser;
use codex_utils_cli::scoped_config_overrides;
use std::ffi::OsString;

pub(super) fn try_parse_from(
    args: impl IntoIterator<Item = impl Into<OsString>>,
) -> Result<MultitoolCli, clap::Error> {
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let mut cli = MultitoolCli::try_parse_from(&args)?;
    if cli.subcommand.is_none() || cli.config_overrides.raw_overrides.is_empty() {
        return Ok(cli);
    }

    // Keep child copies: dispatch applies them after generated feature overrides.
    cli.config_overrides = scoped_config_overrides::<MultitoolCli>(&args)?;
    Ok(cli)
}

#[cfg(test)]
#[path = "config_override_args_tests.rs"]
mod tests;

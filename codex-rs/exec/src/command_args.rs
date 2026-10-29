use std::ffi::OsString;

use clap::Parser;
use codex_utils_cli::scoped_config_overrides;

use crate::TopCli;

pub(super) fn try_parse_from(
    args: impl IntoIterator<Item = impl Into<OsString>>,
) -> Result<TopCli, clap::Error> {
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let mut cli = TopCli::try_parse_from(&args)?;
    if !cli.config_overrides.raw_overrides.is_empty() {
        cli.config_overrides = scoped_config_overrides::<TopCli>(&args)?;
    }
    Ok(cli)
}

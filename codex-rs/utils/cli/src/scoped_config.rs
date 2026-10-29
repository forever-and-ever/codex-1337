use std::ffi::OsString;

use clap::Command;
use clap::CommandFactory;

use crate::CliConfigOverrides;

/// Collects config flags from root to the selected leaf; deeper assignments win.
/// Clap still owns option, prompt and trailing-command parsing, including the separator.
pub fn scoped_config_overrides<C: CommandFactory>(
    args: &[OsString],
) -> Result<CliConfigOverrides, clap::Error> {
    // Global Append replaces earlier scopes instead of combining their values.
    let matches = localize_config_arg(C::command()).try_get_matches_from(args)?;
    let raw_overrides = std::iter::successors(Some(&matches), |scope| {
        scope.subcommand().map(|(_, child)| child)
    })
    .filter_map(|scope| scope.get_many::<String>("raw_overrides"))
    .flatten()
    .cloned()
    .collect();
    Ok(CliConfigOverrides { raw_overrides })
}

fn localize_config_arg(mut command: Command) -> Command {
    if !command
        .get_arguments()
        .any(|arg| arg.get_id() == "raw_overrides")
    {
        command = command.args(CliConfigOverrides::command().get_arguments().cloned());
    }
    command
        .mut_arg("raw_overrides", |arg| arg.global(false))
        .mut_subcommands(localize_config_arg)
}

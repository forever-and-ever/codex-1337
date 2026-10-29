//! Regression coverage for overrides split across CLI command boundaries.

use super::try_parse_from;
use crate::Subcommand;
use crate::mcp_cmd::McpSubcommand;
use pretty_assertions::assert_eq;

#[test]
fn preserves_scoped_overrides_and_forwarded_arguments() {
    let cli = try_parse_from([
        "codex",
        "--config=a=1",
        "mcp",
        "-cb=2",
        "add",
        "example",
        "-c",
        "a=3",
        "--",
        "program",
        "-c",
        "payload",
    ])
    .expect("parse");
    assert_eq!(cli.config_overrides.raw_overrides, ["a=1", "b=2", "a=3"]);
    let Some(Subcommand::Mcp(mcp)) = cli.subcommand else {
        panic!("expected mcp");
    };
    assert_eq!(mcp.config_overrides.raw_overrides, ["a=3"]);
    let McpSubcommand::Add(add) = mcp.subcommand else {
        panic!("expected add");
    };
    assert_eq!(
        add.transport_args.stdio.expect("stdio transport").command,
        ["program", "-c", "payload"],
    );
    let cli = try_parse_from([
        "codex", "-c", "a=1", "exec", "-c", "b=2", "run", "-c", "a=3", "prompt",
    ])
    .expect("parse exec overrides across all scopes");
    let Some(Subcommand::Exec(mut exec)) = cli.subcommand else {
        panic!("expected exec");
    };
    exec.config_overrides
        .prepend_root_overrides(cli.config_overrides);
    assert_eq!(exec.config_overrides.raw_overrides, ["a=1", "b=2", "a=3"]);
}

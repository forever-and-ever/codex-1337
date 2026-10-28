use clap::error::ErrorKind;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn exec_server_help_documents_remote_options() {
    let command = MultitoolCli::command()
        .term_width(80)
        .mut_subcommand("exec-server", |command| {
            command
                .mut_arg("exit_on_stdin_close", |arg| arg.hide_env_values(true))
                .mut_arg("proxy_private_ips_via_upstream", |arg| {
                    arg.hide_env_values(true)
                })
        });
    let help = command
        .try_get_matches_from(["codex", "exec-server", "--help"])
        .expect_err("help should exit before running the executor");
    assert_eq!(help.kind(), ErrorKind::DisplayHelp);
    let help_text = help
        .to_string()
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(help_text);
}
#[test]
fn exec_server_parses_executor_credential_options() {
    let args = [
        "--remote",
        "https://registry.example.com",
        "--environment-id",
        "env-1",
    ];
    for options in [
        vec!["--executor-credential-stdin"],
        vec!["--executor-credential-stdin", "--exit-on-stdin-close"],
    ] {
        let options: Vec<_> = args.iter().copied().chain(options).collect();
        let command = exec_server_from_args(&options);
        assert!(command.executor_credential_stdin);
    }
}

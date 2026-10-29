use super::*;
use pretty_assertions::assert_eq;

#[test]
fn top_cli_parses_resume_prompt_after_config_flag() {
    const PROMPT: &str = "echo resume-with-global-flags-after-subcommand";
    let cli = command_args::try_parse_from([
        "codex-exec",
        "-c",
        "openai_base_url=\"http://127.0.0.1:1234/v1\"",
        "resume",
        "--strict-config",
        "--last",
        "--json",
        "--model",
        "gpt-5.2-codex",
        "--config",
        "reasoning_level=xhigh",
        "--dangerously-bypass-approvals-and-sandbox",
        "--skip-git-repo-check",
        PROMPT,
    ])
    .expect("parse scoped overrides");
    let mut inner = cli.inner;
    inner
        .config_overrides
        .prepend_root_overrides(cli.config_overrides);

    let codex_exec::Command::Resume(args) = &inner.command else {
        panic!("expected resume command");
    };
    let effective_prompt = args.prompt.clone().or_else(|| {
        if args.last {
            args.session_id.clone()
        } else {
            None
        }
    });
    assert_eq!(effective_prompt.as_deref(), Some(PROMPT));
    // Global flags on both sides of the verb must not lose a mock/provider route.
    assert_eq!(
        inner.config_overrides.raw_overrides,
        [
            "openai_base_url=\"http://127.0.0.1:1234/v1\"",
            "reasoning_level=xhigh"
        ]
    );
    assert!(inner.strict_config);
}

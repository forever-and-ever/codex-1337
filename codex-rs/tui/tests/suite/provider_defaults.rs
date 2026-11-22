//! Resume and fork discover history across providers through the real CLI and app server.

use super::focus_palette::PtyCodex;
use super::focus_palette::socket_home;
use super::focus_palette::write_test_config;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use app_test_support::create_fake_rollout;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_lookup_spans_providers_with_local_and_embedded_servers() -> Result<()> {
    let cwd = codex_utils_cargo_bin::repo_root()?;
    for action in ["resume", "fork"] {
        // Cover each server topology and both lookup paths without a duplicate subprocess grid.
        for (embedded, last, explicit_remote) in [
            (false, false, false),
            (true, true, false),
            (false, true, true),
        ] {
            let home = socket_home()?;
            write_test_config(home.path(), &cwd)?;
            let config_path = home.path().join("config.toml");
            let config = std::fs::read_to_string(&config_path)?.replace(
                "features.daemon_auto_start = false",
                "features.daemon_auto_start = false\nfeatures.api_key_model_discovery = false",
            );
            std::fs::write(
                config_path,
                format!(
                    "{config}\n[model_providers.server-provider]\nname = 'Server provider'\nbase_url = 'http://127.0.0.1:9/v1'\nwire_api = 'responses'\nrequires_openai_auth = false\n[projects.'/']\ntrust_level = 'trusted'\n"
                ),
            )?;
            for (hour, provider, preview) in [
                ("10", "openai", "Client provider history"),
                ("11", "server-provider", "Server provider history"),
            ] {
                create_fake_rollout(
                    home.path(),
                    &format!("2025-01-02T{hour}-00-00"),
                    &format!("2025-01-02T{hour}:00:00Z"),
                    preview,
                    Some(provider),
                    /*git_info*/ None,
                )?;
            }
            let (mut daemon, remote_endpoint) = super::unix_daemon::start(
                home.path(),
                &["-c", "model_provider=\"server-provider\""],
            )
            .await?;
            let mut args = vec![action, "--all"];
            if embedded {
                args.push("--no-daemon");
            }
            if last {
                args.push("--last");
            }
            if explicit_remote {
                // Client config already defaults to OpenAI; do not request a forbidden provider
                // mutation while checking history discovery across the saved providers.
                args.extend(["--remote", remote_endpoint.as_str()]);
            }
            let mut terminal = PtyCodex::start_cli(&cwd, home, &args)?;
            terminal
                .wait_for_screen("Server provider history")
                .with_context(|| format!("provider history invocation: {args:?}"))?;
            if last {
                ensure!(
                    !terminal.screen_contains("Client provider history"),
                    "{args:?}: --last did not select the newest conversation; screen:\n{}",
                    terminal.screen_contents()
                );
            } else {
                terminal.wait_for_screen("Client provider history")?;
            }
            drop(terminal);
            daemon.kill().await?;
            daemon.wait().await?;
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_gateway_authenticates_before_its_first_chat() -> Result<()> {
    use core_test_support::responses;

    let server = responses::start_mock_server().await;
    let reply = responses::mount_sse_once_match(
        &server,
        wiremock::matchers::body_string_contains(r#"\"thread_source\":\"user\""#),
        responses::sse(vec![
            responses::ev_assistant_message("msg", "gateway handoff verified"),
            responses::ev_completed("resp"),
        ]),
    )
    .await;
    let workspace = tempfile::tempdir()?;
    let home = tempfile::tempdir()?;
    let cwd = workspace.path().canonicalize()?;
    let profile = home.path().join("work.config.toml");
    let auth = home.path().join("auth.json");
    let url = server.uri();
    let trusted = toml::Value::String(cwd.display().to_string());
    std::fs::write(
        &profile,
        format!(
            r#"
model_provider = "openai"
model = "gpt-5.1"
forced_login_method = "api"
cli_auth_credentials_store = "file"
suppress_unstable_features_warning = true
[features]
gateway_setup_wizard = true
api_key_model_discovery = false
[projects.{trusted}]
trust_level = "trusted"
[model_providers.office]
name = "Office gateway"
base_url = "{url}/v1"
wire_api = "responses"
supports_websockets = false
requires_openai_auth = true
"#
        ),
    )?;
    let mut terminal = PtyCodex::start(
        &cwd,
        home,
        &[
            "--profile",
            "work",
            "--no-daemon",
            "--prompt",
            "verify the gateway",
        ],
    )?;
    terminal
        .wait_for_screen("Sign in another way")
        .with_context(|| terminal.screen_contents())?;
    // Startup drains input after its first draw. Retry the menu shortcut until it is accepted.
    let deadline = std::time::Instant::now() + Duration::from_secs(/*secs*/ 30);
    while !terminal.screen_contains("Use a custom provider or gateway") {
        ensure!(
            std::time::Instant::now() < deadline,
            "gateway option missing: {}",
            terminal.screen_contents()
        );
        terminal.write_input(b"2")?;
        terminal.read_output(Duration::from_millis(/*millis*/ 50))?;
    }
    // Arrow navigation avoids carrying the menu shortcut across two screens.
    terminal.write_input(b"\x1b[B\r")?;
    terminal.wait_for_screen("Office gateway")?;
    terminal.write_input(b"\x1b[H\r")?;
    terminal.wait_for_screen("Paste or type your API key")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(/*secs*/ 30);
    while !auth.is_file() && terminal.screen_contains("Paste or type your API key") {
        ensure!(
            std::time::Instant::now() < deadline,
            "API key was not saved: {}",
            terminal.screen_contents()
        );
        terminal.write_input(b"\r")?;
        terminal.read_output(Duration::from_millis(/*millis*/ 50))?;
    }
    terminal.wait_for_screen("gateway handoff verified")?;
    let first_chat = reply
        .requests()
        .into_iter()
        .find(|request| {
            request
                .message_input_texts("user")
                .iter()
                .any(|text| text == "verify the gateway")
        })
        .context("the selected gateway received the first chat")?;
    assert_eq!(
        first_chat.header("authorization").as_deref(),
        Some("Bearer focus-palette-test")
    );
    Ok(())
}

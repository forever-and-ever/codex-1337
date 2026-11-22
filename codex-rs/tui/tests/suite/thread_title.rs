//! Exercises automatic title generation alongside a foreground skill installation prompt.

use super::focus_palette::PtyCodex;
use super::focus_palette::write_test_config;
use anyhow::Result;
use core_test_support::responses;
use std::io::Write;
use std::time::Duration;
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_title_after_continue_anyway() -> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let workspace = tempfile::tempdir()?;
    let cwd = workspace.path().canonicalize()?;
    let home = tempfile::tempdir()?;
    write_test_config(home.path(), &cwd)?;
    let skill = home.path().join("skills/ping");
    std::fs::create_dir_all(skill.join("agents"))?;
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: ping\ndescription: Ping a service.\n---\nCheck the service.\n",
    )?;
    std::fs::write(
        skill.join("agents/openai.yaml"),
        "dependencies:
  tools:
    - type: mcp
      value: ping
      transport: streamable_http
      url: http://127.0.0.1:1/mcp
",
    )?;

    let server = responses::start_mock_server().await;
    responses::mount_response_handler(&server, |request| {
        let (id, message) = if request.body_json()["text"]["format"]["type"] == "json_schema" {
            ("title", r#"{"title":"Check service health"}"#)
        } else {
            ("foreground", "Foreground turn completed")
        };
        responses::sse_response(responses::sse(vec![
            responses::ev_assistant_message(id, message),
            responses::ev_completed(id),
        ]))
    })
    .await;
    let mut config = std::fs::OpenOptions::new()
        .append(true)
        .open(home.path().join("config.toml"))?;
    writeln!(
        config,
        "[model_providers.test]
name = 'Mock'
base_url = '{}/v1'
wire_api = 'responses'

[tui]
status_line = ['thread-title']",
        server.uri(),
    )?;
    let mut terminal = PtyCodex::start(
        &cwd,
        home,
        &[
            "--no-daemon",
            "-c",
            "model_provider='test'",
            "-c",
            "approval_policy='on-request'",
            "--prompt",
            "$ping the service",
        ],
    )?;
    terminal.wait_for_screen("Continue anyway")?;
    // Startup can drain pending input after it first draws the installation prompt.
    let deadline = Instant::now() + Duration::from_secs(/*secs*/ 30);
    while terminal.screen_contains("1 unanswered") {
        anyhow::ensure!(Instant::now() < deadline, "installation prompt stayed open");
        terminal.write_input(b"2")?;
        terminal.read_output(Duration::from_millis(/*millis*/ 50))?;
    }
    terminal.wait_for_screen("Foreground turn completed")?;
    terminal.wait_for_screen("Check service health")?;

    // The fork presents answered requests as collapsed command results.
    let screen = terminal.screen_contents();
    let (column, row) = screen
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("Command"))
        .find_map(|(row, line)| {
            let (before, _) = line.split_once("[out]")?;
            Some((before.width() + 1, row + 1))
        })
        .ok_or_else(|| anyhow::anyhow!("missing command result control; screen:\n{screen}"))?;
    terminal.write_input(format!("\x1b[<0;{column};{row}M\x1b[<0;{column};{row}m").as_bytes())?;
    terminal.wait_for_screen("answer: Continue anyway")?;
    assert!(
        !terminal.screen_contains("temporary structured threads cannot request"),
        "{}",
        terminal.screen_contents()
    );
    Ok(())
}

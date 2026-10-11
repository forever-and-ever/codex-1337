use anyhow::Result;
use codex_features::Feature;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_custom_tool_call;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;

use super::custom_tool_output_items;
use super::text_item;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filesystem_authority_and_transfer_bounds() -> Result<()> {
    skip_if_no_network!(Ok(()));
    core_test_support::skip_if_sandbox!(Ok(()));
    let server = responses::start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        let _ = config.features.enable(Feature::CodeModeOnly);
    });
    let test = builder.build_with_auto_env(&server).await?;
    for (name, bytes) in [
        ("seed", b"private result".to_vec()),
        ("oversize", vec![0; 1024 * 1024 + 1]),
    ] {
        test.fs()
            .write_file(
                &test.workspace_path_uri(name)?,
                bytes,
                Default::default(),
                /*sandbox*/ None,
            )
            .await?;
    }
    let log = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_custom_tool_call(
                    "filesystem",
                    "exec",
                    r#"
if (await fs.readFile('seed', 'utf8') !== 'private result') throw new Error('wrong executor');
let denied = false;
try { await fs.writeFile('seed', 'corrupted'); } catch (_) { denied = true; }
if (!denied) throw new Error('sandbox bypass');
let bounded = false;
try { await fs.readFile('oversize'); } catch (error) { bounded = String(error).includes('1 MiB'); }
if (!bounded) throw new Error('read was unbounded or silently truncated');
text('authority and bounds preserved');
"#,
                ),
                ev_completed("fs"),
            ]),
            sse(vec![
                ev_assistant_message("done", "done"),
                ev_completed("done"),
            ]),
        ],
    )
    .await;
    test.codex
        .start_or_steer_turn(
            codex_core::TurnInputRequest::user_input(vec![
                codex_protocol::user_input::UserInput::Text {
                    text: "Read files under read-only authority".into(),
                    text_elements: Vec::new(),
                },
            ])
            .with_thread_settings(codex_protocol::protocol::ThreadSettingsOverrides {
                approval_policy: Some(codex_protocol::protocol::AskForApproval::Never),
                sandbox_policy: Some(codex_protocol::protocol::SandboxPolicy::ReadOnly {
                    network_access: false,
                }),
                ..Default::default()
            }),
        )
        .await?;
    core_test_support::wait_for_event(&test.codex, |event| {
        matches!(event, codex_protocol::protocol::EventMsg::TurnComplete(_))
    })
    .await;
    let request = log.last_request().expect("follow-up");
    let output = custom_tool_output_items(&request, "filesystem");
    assert_eq!(text_item(&output, 1), "authority and bounds preserved");
    assert_eq!(
        test.fs()
            .read_file(
                &test.workspace_path_uri("seed")?,
                Default::default(),
                /*sandbox*/ None
            )
            .await?,
        b"private result"
    );
    Ok(())
}

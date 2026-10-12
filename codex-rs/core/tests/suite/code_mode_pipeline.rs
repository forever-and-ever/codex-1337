use anyhow::Result;
use codex_features::Feature;
use core_test_support::mock_server;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_custom_tool_call;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use test_case::test_case;

use super::custom_tool_output_items;
use super::text_item;

// The stages are emitted in a single response. A lock-only fence can race eager
// dispatch; a coordinator holding that lock deadlocks its own nested command.
#[test_case("python", "import pathlib\npathlib.Path('input').write_text('40')", true; "python_pipeline")]
#[test_case("bash", "printf 40 > input", true; "bash_pipeline")]
#[test_case("python", "raise RuntimeError('producer failed')", false; "process_failure")]
#[test_case("exec", "await tools.exec_command({cmd: 'exit 7'});", false; "nested_failure")]
#[test_case("exec", "try { await fs.readFile('missing'); } catch (_) {}", false; "caught_failure")]
#[test_case("bash", "# @exec: {\"background\":true}\ntrue", false; "explicit_background")]
#[test_case("exec", "yield_control(); await new Promise(r => setTimeout(r, 100));", false; "yielded_cell")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_stage_pipeline(language: &str, source: &str, success: bool) -> Result<()> {
    core_test_support::skip_if_target_windows!(Ok(()), "POSIX Bash and python3 fixtures");
    skip_if_no_network!(Ok(()));
    core_test_support::skip_if_sandbox!(Ok(()));
    let server = responses::start_mock_server().await;
    mock_server::run(&[&server], async {
    let mut builder = test_codex().with_config(|config| {
        let _ = config.features.enable(Feature::CodeModeOnly);
    });
    let test = builder.build_with_auto_env(&server).await?;
    let log = responses::mount_sse_sequence(&server, vec![
        sse(vec![
            ev_response_created("pipeline"),
            ev_custom_tool_call("produce", language, source),
            ev_custom_tool_call("independent", "bash", "printf 2 > increment"),
            ev_function_call("barrier-a", "fence", "{}"),
            ev_custom_tool_call("consume", "exec", r#"
const a = Number(await fs.readFile('input', 'utf8'));
const b = Number(await fs.readFile('increment', {encoding: 'utf8'}));
await fs.writeFile('result', String(a + b));
const result = await tools.exec_command({cmd: 'cat result'});
if (result.output !== '42') throw new Error('pipeline corrupted');
text(result.output);
"#),
            ev_function_call("barrier-b", "fence", "{}"),
            ev_custom_tool_call("final", "python", "from pathlib import Path\nassert Path('result').read_text() == '42'\nPath('finished').write_text('done')"),
            ev_completed("pipeline"),
        ]),
        sse(vec![ev_assistant_message("done", "done"), ev_completed("done")]),
    ]).await;
    test.submit_turn("Run the staged pipeline in one batch")
        .await?;
    let requests = log.requests();
    assert_eq!(
        requests.len(),
        2,
        "the pipeline requires no intermediate inference"
    );
    let followup = &requests[1];
    let barrier = followup.function_call_output("barrier-a");
    let final_output = followup.custom_tool_call_output("final");
    if success {
        assert_eq!(barrier["output"], "Fence passed");
        let items = custom_tool_output_items(followup, "consume");
        assert_eq!(text_item(&items, 1), "42");
        assert_eq!(
            test.fs()
                .read_file(
                    &test.workspace_path_uri("finished")?,
                    Default::default(),
                    /*sandbox*/ None
                )
                .await?,
            b"done"
        );
    } else {
        assert!(
            barrier["output"]
                .as_str()
                .unwrap()
                .contains("Skipped by fence"),
            "{barrier}"
        );
        assert!(
            final_output["output"]
                .as_str()
                .unwrap()
                .contains("Skipped by fence"),
            "{final_output}"
        );
        assert!(
            test.fs()
                .read_file(
                    &test.workspace_path_uri("finished")?,
                    Default::default(),
                    /*sandbox*/ None
                )
                .await
                .is_err()
        );
        assert!(
            test.fs()
                .read_file(
                    &test.workspace_path_uri("result")?,
                    Default::default(),
                    /*sandbox*/ None
                )
                .await
                .is_err()
        );
    }
    Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filesystem_authority_and_transfer_bounds() -> Result<()> {
    skip_if_no_network!(Ok(()));
    core_test_support::skip_if_sandbox!(Ok(()));
    let server = responses::start_mock_server().await;
    mock_server::run(&[&server], async {
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
                    ev_function_call("barrier", "fence", "{}"),
                    ev_custom_tool_call("skipped", "exec", "text('must not execute');"),
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
                .with_thread_settings(
                    codex_protocol::protocol::ThreadSettingsOverrides {
                        approval_policy: Some(codex_protocol::protocol::AskForApproval::Never),
                        sandbox_policy: Some(codex_protocol::protocol::SandboxPolicy::ReadOnly {
                            network_access: false,
                        }),
                        ..Default::default()
                    },
                ),
            )
            .await?;
        core_test_support::wait_for_event(&test.codex, |event| {
            matches!(event, codex_protocol::protocol::EventMsg::TurnComplete(_))
        })
        .await;
        let request = log.last_request().expect("follow-up");
        let output = custom_tool_output_items(&request, "filesystem");
        assert_eq!(text_item(&output, 1), "authority and bounds preserved");
        assert!(
            request.custom_tool_call_output("skipped")["output"]
                .as_str()
                .unwrap()
                .contains("Skipped by fence")
        );
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
    })
    .await
}

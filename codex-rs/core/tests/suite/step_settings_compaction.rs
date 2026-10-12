//! Model requests retain the summary captured after a mid-turn settings update and compaction.

use super::*;
use codex_history::RolloutItem;
use core_test_support::mock_server;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use test_case::test_case;

#[derive(Clone, Copy)]
enum CompactionMode {
    Local,
    RemoteV2,
}

#[test_case(CompactionMode::Local; "local")]
#[test_case(CompactionMode::RemoteV2; "remote v2")]
#[tokio::test]
async fn compaction_preserves_updated_summary(mode: CompactionMode) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    mock_server::run(&[&server], async {
    let mut bodies = vec![
        paused_response("pause", "pause-call"),
        sse(vec![
            ev_function_call(
                "plan",
                "update_plan",
                &json!({"plan": [{"step": "Finish the answer", "status": "in_progress"}]})
                    .to_string(),
            ),
            responses::ev_completed_with_tokens("plan-response", /*total_tokens*/ 330_000),
        ]),
    ];
    bodies.push(match mode {
        CompactionMode::Local => sse(vec![
            responses::ev_assistant_message("summary", "Compacted history"),
            ev_completed("compact"),
        ]),
        CompactionMode::RemoteV2 => sse(vec![
            json!({"type": "response.output_item.done", "item": {"type": "compaction", "encrypted_content": "encrypted-summary"}}),
            ev_completed("compact"),
        ]),
    });
    bodies.push(sse_completed("done"));
    let responses = mount_sse_sequence(&server, bodies).await;
    let test = step_settings_test()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            config.model_auto_compact_token_limit = Some(200_000);
            config
                .features
                .disable(Feature::TokenBudget)
                .expect("disable token budget");
            config.model_provider.name = match mode {
                CompactionMode::Local => "Local compaction test",
                CompactionMode::RemoteV2 => "OpenAI",
            }
            .to_string();
        })
        .build_with_auto_env(&server)
        .await?;
    const OLD: &str = "standing-instruction-before-amendment";
    const CURRENT: &str = "standing-instruction-after-amendment";
    let agents = test.home.path().join("AGENTS.md");
    std::fs::write(&agents, OLD)?;
    let pause = start_paused_turn(&test.codex).await?;
    std::fs::write(&agents, CURRENT)?;
    assert_eq!(
        submit_turn_settings(
            &test.codex,
            &pause.turn_id,
            TurnSettingsUpdate {
                summary: Some(ReasoningSummary::Detailed),
                ..Default::default()
            }
        )
        .await?,
        TurnSettingsUpdateOutcome::Applied
    );
    answer_paused_turn(&test.codex, &pause.turn_id).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    test.codex.shutdown_and_wait().await?;

    let requests = responses.requests();
    assert_eq!(requests.len(), 4);
    // Live requests are append-only. The compactor alone sees a materialized
    // instruction view, and its replacement window starts from that same full value.
    let first = serde_json::to_string(&requests[0].input())?;
    let live = serde_json::to_string(&requests[1].input())?;
    assert!(first.contains(OLD));
    assert!(live.contains(OLD) && live.contains(CURRENT) && live.contains("<instruction_diff>"));
    assert_eq!(
        requests[1].input().get(..requests[0].input().len()),
        Some(requests[0].input().as_slice())
    );
    for request in &requests[2..] {
        let materialized = serde_json::to_string(&request.input())?;
        assert!(materialized.contains(CURRENT));
        assert!(!materialized.contains(OLD));
        assert!(!materialized.contains("<instruction_diff>"));
    }
    assert_eq!(
        [
            requests[0].body_json()["reasoning"]["summary"].clone(),
            requests[1].body_json()["reasoning"]["summary"].clone(),
            requests[3].body_json()["reasoning"]["summary"].clone(),
        ],
        [json!("concise"), json!("detailed"), json!("detailed")]
    );
    let rollout =
        std::fs::read_to_string(test.session_configured.rollout_path.expect("rollout path"))?;
    let items = rollout
        .lines()
        .map(codex_rollout::parse_rollout_line)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let preambles = items
        .iter()
        .filter_map(|line| match &line.item {
            RolloutItem::EventMsg(EventMsg::ItemCompleted(event)) => match &event.item {
                codex_protocol::items::TurnItem::Preamble(item) => Some(item),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(preambles.len(), 2);
    assert_ne!(preambles[0].window_id, preambles[1].window_id);
    assert!(
        preambles[0]
            .raw
            .as_ref()
            .is_some_and(|raw| raw.contains(OLD) && !raw.contains(CURRENT))
    );
    assert!(
        preambles[1]
            .raw
            .as_ref()
            .is_some_and(|raw| raw.contains(CURRENT) && !raw.contains(OLD))
    );
    let checkpoint = items
        .iter()
        .skip_while(|line| !matches!(line.item, RolloutItem::Compacted(_)))
        .skip(/*n*/ 1)
        .take_while(|line| !matches!(line.item, RolloutItem::EventMsg(_)))
        .find_map(|line| match &line.item {
            RolloutItem::TurnContext(context) => Some(context.summary),
            _ => None,
        });
    assert_eq!(checkpoint, Some(Some(ReasoningSummary::None)));
    Ok(())
    })
    .await
}

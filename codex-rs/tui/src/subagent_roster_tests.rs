use super::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

#[test]
fn roster_model_columns() -> anyhow::Result<()> {
    let watched = ThreadId::from_u128(/*value*/ 1);
    let view = RosterView {
        roster: Arc::new(Mutex::new(SubagentRoster {
            rows: vec![
                AgentRow {
                    id: watched,
                    name: "alpha".into(),
                    status: Some(ThreadStatus::Idle),
                    activity: Some("Inspect parser".into()),
                    tokens: Some(1_500_000),
                    model: Some(AgentModel {
                        model: "gpt-6.1-sol".into(),
                        effort: Some(ReasoningEffort::Low),
                    }),
                },
                AgentRow {
                    id: ThreadId::from_u128(/*value*/ 2),
                    name: "worker".into(),
                    status: Some(ThreadStatus::Idle),
                    activity: None,
                    tokens: None,
                    model: Some(AgentModel {
                        model: "claude-sonnet-4-6".into(),
                        effort: None,
                    }),
                },
                AgentRow {
                    id: ThreadId::from_u128(/*value*/ 3),
                    name: "worker".into(),
                    status: Some(ThreadStatus::Idle),
                    activity: Some("Inspect parser".into()),
                    tokens: None,
                    model: None,
                },
            ],
            watched: Some(watched),
            ..Default::default()
        })),
        use_theme_colors: false,
    };
    let mut snapshots = Vec::new();
    for height in [4, 2] {
        let mut terminal = Terminal::new(TestBackend::new(/*width*/ 80, height))?;
        let _ = terminal.draw(|frame| view.render(frame.area(), frame.buffer_mut()))?;
        snapshots.push(terminal.backend().to_string());
    }
    insta::assert_snapshot!(snapshots.join("\n\n"));
    Ok(())
}

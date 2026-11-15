//! The live capture warpaint's packs are judged on: a fabricated conversation, driven through the
//! App as the app server drives it and drawn full screen, then exported before its frame binds.
//! `just warpaint-capture` runs it.

use super::*;
use crate::transcript_mode::TranscriptMode;
use codex_app_server_protocol::ArrivalItem;
use codex_app_server_protocol::CommandAction;
use codex_app_server_protocol::CommandExecutionApprovalKind;
use codex_app_server_protocol::CommandExecutionRequestApprovalParams;
use codex_app_server_protocol::CommandExecutionSource;
use codex_app_server_protocol::CommandExecutionStatus;
use codex_app_server_protocol::FileUpdateChange;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::PatchApplyStatus;
use codex_app_server_protocol::PatchChangeKind;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_app_server_protocol::WarningNotification;
use codex_protocol::ToolName;
use codex_protocol::step_admission::ArrivalKind;
use codex_protocol::step_admission::ArrivalSource;
use codex_protocol::tool_input::ToolInput;
use codex_protocol::tool_input::ToolInputPayload;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Position;

const SIZE: Size = Size {
    width: 100,
    height: 60,
};
const THREAD: &str = "00000000-0000-0000-0000-00000000c0de";
const TURN: &str = "turn-1";
/// 2025-03-14 09:41 UTC: a past year, so the completion label keeps its date whenever it is drawn.
const STARTED_AT: i64 = 1_741_945_260;

#[test]
#[ignore = "rewrites scripts/warpaint/captures; run through `just warpaint-capture`"]
fn conversation() -> Result<()> {
    assert!(
        [("TZ", "UTC"), ("LC_ALL", "C.UTF-8"), ("FORCE_COLOR", "3")]
            .iter()
            .all(|(name, value)| std::env::var(name).is_ok_and(|set| set == *value)),
        "clocks and color depth follow the host; run through `just warpaint-capture`"
    );
    crate::warpaint::capture::invested(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(drive())
    })
}

async fn drive() -> Result<()> {
    let (mut app, mut events, _ops) = Box::pin(tests::make_test_app_with_channels()).await;
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = tui::test_support::make_test_tui()?;
    // Every geometry query answers `SIZE`, as a real terminal's would.
    tui.terminal = crate::custom_terminal::Terminal::with_screen_size_and_cursor_position_for_test(
        CrosstermBackend::new(crate::custom_terminal::output::stdout()),
        SIZE,
        Position::new(/*x*/ 0, /*y*/ 0),
    );
    tui.set_owned_screen(/*owned*/ true)?;
    for settings in [&mut app.local_settings, &mut app.chat_widget.local_settings] {
        settings.tui.animations = false;
        settings.transcript_mode = TranscriptMode::Owned;
    }
    // The status row reads its motion from the bottom pane, which a setting reaches only as the
    // config panel applies it.
    let still = app.chat_widget.local_settings.tui.clone();
    app.chat_widget.apply_config_preference(
        crate::config_panel::ConfigPreference::Toggle {
            setting: crate::config_panel::ConfigToggle::Animations,
            enabled: false,
        },
        &still,
    );
    app.chat_widget
        .handle_thread_session_quiet(ThreadSessionState {
            model: app.chat_widget.current_model().to_string(),
            ..tests::test_thread_session(
                ThreadId::from_string(THREAD)?,
                app.config.cwd.to_path_buf(),
            )
        });
    let mut session = Session {
        app: &mut app,
        tui: &mut tui,
        server: &mut server,
        events: &mut events,
    };
    for notification in script(THREAD, &session.app.config.cwd) {
        let rerun = matches!(
            &notification,
            ServerNotification::ItemStarted(ItemStartedNotification {
                item: codex_app_server_protocol::ThreadItem {
                    payload: ThreadItemPayload::CommandExecution { id, .. }, ..
                },
                ..
            }) if id == "exec-2"
        );
        session
            .app
            .chat_widget
            .handle_server_notification(notification, /*replay_kind*/ None);
        session.settle().await?;
        // While the rerun runs, the turn is at work: its status line and the keys that stop it.
        if rerun {
            session.capture("working")?;
        }
    }
    // Open the patch, and the answer's reasoning from its response header, as a reader would.
    session.open_fold("src/parser.rs").await?;
    session.click("[cot]").await?;
    // Escape returns focus to the composer.
    session.input(TuiEvent::Key(KeyCode::Esc.into())).await?;
    // `/copy` selects the latest response; `/theme` lists the packs over the conversation.
    session.command("/copy", "selection").await?;
    session.command("/theme", "picker").await?;
    // `?` in the empty composer lists the shortcuts, and puts them away again.
    let shortcuts = TuiEvent::Key(KeyCode::Char('?').into());
    session.input(shortcuts.clone()).await?;
    session.capture("shortcuts")?;
    session.input(shortcuts).await?;
    session.app.chat_widget.insert_str(DRAFT);
    session.capture("conversation")?;
    // The same transcript in detail, where tool output keeps its terminal colors.
    let detail = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
    session.input(TuiEvent::Key(detail)).await?;
    session.capture("detail")?;
    // Back on the conversation, the agent asks to run the whole workspace's tests. Approvals
    // wait out a second of typing, lest a keystroke answer them.
    session.input(TuiEvent::Key(detail)).await?;
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    session
        .app
        .chat_widget
        .handle_server_request(approval(THREAD), /*replay_kind*/ None);
    session.settle().await?;
    session.capture("approval")?;
    tui.set_owned_screen(/*owned*/ false)?;
    server.shutdown().await?;
    Ok(())
}

/// The App on its terminal, and the events it sends itself.
struct Session<'a> {
    app: &'a mut App,
    tui: &'a mut tui::Tui,
    server: &'a mut AppServerSession,
    events: &'a mut mpsc::UnboundedReceiver<AppEvent>,
}

impl Session<'_> {
    /// Delivers terminal input, then the events it caused, as the App's loop would.
    async fn input(&mut self, event: TuiEvent) -> Result<()> {
        self.app
            .handle_tui_event(self.tui, self.server, event)
            .await?;
        self.settle().await
    }

    async fn settle(&mut self) -> Result<()> {
        while let Ok(event) = self.events.try_recv() {
            Box::pin(self.app.handle_event(self.tui, self.server, event)).await?;
        }
        Ok(())
    }

    /// Runs a slash command, captures the screen it opens as `frame`, and closes it.
    async fn command(&mut self, command: &str, frame: &str) -> Result<()> {
        self.app.chat_widget.insert_str(command);
        self.input(TuiEvent::Key(KeyCode::Enter.into())).await?;
        self.capture(frame)?;
        self.input(TuiEvent::Key(KeyCode::Esc.into())).await
    }

    fn draw(&mut self) -> Result<Rect> {
        self.app.render_owned_transcript(self.tui, SIZE)
    }

    fn capture(&mut self, frame: &str) -> Result<()> {
        let (drawn, unbound) = crate::warpaint::capture::unbound(|| self.draw());
        drawn?;
        crate::warpaint::capture::write(frame, &unbound)?;
        Ok(())
    }

    /// Clicks where `text` first starts on the screen as drawn now.
    async fn click(&mut self, text: &str) -> Result<()> {
        self.draw()?;
        let (column, row) = self.find(text);
        self.click_at(column, row).await
    }

    /// Clicks the fold on the row where `text` first starts: its triangle in the control column.
    async fn open_fold(&mut self, text: &str) -> Result<()> {
        self.draw()?;
        let (_, row) = self.find(text);
        self.click_at(crate::transcript_geometry::CONTROL_COLUMN, row)
            .await
    }

    async fn click_at(&mut self, column: u16, row: u16) -> Result<()> {
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            let click = MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            };
            self.input(TuiEvent::Mouse(click)).await?;
        }
        Ok(())
    }

    /// Where `text` first starts on screen.
    fn find(&self, text: &str) -> (u16, u16) {
        let screen = crate::custom_terminal::test_support::last_rendered_buffer(&self.tui.terminal);
        screen
            .content
            .chunks(usize::from(screen.area.width))
            .zip(0..)
            .find_map(|(cells, row)| {
                let symbols: Vec<&str> = cells.iter().map(ratatui::buffer::Cell::symbol).collect();
                (0..screen.area.width)
                    .find(|&column| symbols[usize::from(column)..].concat().starts_with(text))
                    .map(|column| (column, row))
            })
            .unwrap_or_else(|| panic!("{text:?} is not on screen"))
    }
}

fn script(thread: &str, cwd: &AbsolutePathBuf) -> Vec<ServerNotification> {
    let at = |seconds: i64| (STARTED_AT + seconds) * 1_000;
    let started = |seconds, item| {
        ServerNotification::ItemStarted(ItemStartedNotification::new(
            thread.to_string(),
            TURN.to_string(),
            item,
            at(seconds),
        ))
    };
    let completed = |seconds, item| {
        ServerNotification::ItemCompleted(ItemCompletedNotification::new(
            thread.to_string(),
            TURN.to_string(),
            item,
            None,
            at(seconds),
        ))
    };
    let turn = |status, completed_at: Option<i64>| Turn {
        id: TURN.to_string(),
        root_turn_id: None,
        items_view: TurnItemsView::Full,
        items: Vec::new(),
        status,
        error: None,
        started_at: Some(STARTED_AT),
        completed_at: completed_at.map(|seconds| STARTED_AT + seconds),
        duration_ms: completed_at.map(|seconds| seconds * 1_000),
    };
    let input = |id: &str| {
        ThreadItemPayload::ToolInput(ToolInput {
            id: format!("input:{id}"),
            call_id: id.to_string(),
            name: ToolName::plain("exec_command"),
            payload: ToolInputPayload::Json(format!(r#"{{"cmd":"{COMMAND}"}}"#)),
        })
    };
    let exec =
        |id: &str, status, output: Option<&str>, exit_code| ThreadItemPayload::CommandExecution {
            model_context: None,
            sandbox_type: None,
            id: id.to_string(),
            command: COMMAND.to_string(),
            cwd: cwd.clone().into(),
            process_id: None,
            plugin_id: None,
            script_path: None,
            source: CommandExecutionSource::Agent,
            status,
            command_actions: vec![CommandAction::Unknown {
                command: COMMAND.to_string(),
            }],
            aggregated_output: output.map(str::to_string),
            exit_code,
            duration_ms: exit_code.map(|_| 1_840),
        };
    let patch = |status| ThreadItemPayload::FileChange {
        id: "patch-1".to_string(),
        changes: vec![FileUpdateChange {
            path: "src/parser.rs".to_string(),
            kind: PatchChangeKind::Update { move_path: None },
            diff: PATCH.to_string(),
        }],
        status,
    };
    vec![
        ServerNotification::TurnStarted(TurnStartedNotification {
            thread_id: thread.to_string(),
            turn: turn(TurnStatus::InProgress, None),
        }),
        completed(
            0,
            ThreadItemPayload::UserMessage {
                source_response_item_id: None,
                id: "prompt-1".to_string(),
                client_id: None,
                content: vec![UserInput::Text {
                    text: PROMPT.to_string(),
                    text_elements: Vec::new(),
                }],
            },
        ),
        completed(3, input("exec-1")),
        started(
            3,
            exec("exec-1", CommandExecutionStatus::InProgress, None, None),
        ),
        completed(
            5,
            exec(
                "exec-1",
                CommandExecutionStatus::Failed,
                Some(FAILING),
                Some(101),
            ),
        ),
        started(12, patch(PatchApplyStatus::InProgress)),
        completed(12, patch(PatchApplyStatus::Completed)),
        completed(20, input("exec-2")),
        started(
            20,
            exec("exec-2", CommandExecutionStatus::InProgress, None, None),
        ),
        completed(
            22,
            exec(
                "exec-2",
                CommandExecutionStatus::Completed,
                Some(PASSING),
                Some(0),
            ),
        ),
        completed(
            30,
            ThreadItemPayload::Arrival(ArrivalItem {
                id: "arrival-1".to_string(),
                response_id: "response-1".to_string(),
                announced_at_ms: at(30),
                kind: ArrivalKind::InterAgent,
                label: "reviewer".to_string(),
                text: None,
                source: ArrivalSource::Captured {
                    blocks: vec![SUBAGENT.to_string()],
                },
                placement: None,
            }),
        ),
        ServerNotification::Warning(WarningNotification {
            thread_id: Some(thread.to_string()),
            message: WARNING.to_string(),
        }),
        completed(
            41,
            ThreadItemPayload::AgentMessage {
                id: "answer-1".to_string(),
                text: ANSWER.to_string(),
                phase: None,
                memory_citation: None,
                moderation: None,
                delivery: None,
                questions: None,
            },
        ),
        // A response's thought follows its members, as the server announces it.
        completed(
            41,
            ThreadItemPayload::Thought {
                id: "thought-1".to_string(),
                response_id: "response-2".to_string(),
                summary: Vec::new(),
                content: vec![REASONING.to_string()],
                reported_reasoning_tokens: None,
            },
        ),
        ServerNotification::TurnCompleted(TurnCompletedNotification {
            thread_id: thread.to_string(),
            turn: turn(TurnStatus::Completed, Some(42)),
        }),
    ]
}

/// The agent's request to run `APPROVAL`, as the app server sends it.
fn approval(thread: &str) -> ServerRequest {
    ServerRequest::CommandExecutionRequestApproval {
        request_id: RequestId::Integer(1),
        params: CommandExecutionRequestApprovalParams {
            kind: CommandExecutionApprovalKind::Command,
            thread_id: thread.to_string(),
            turn_id: TURN.to_string(),
            item_id: "exec-3".to_string(),
            started_at_ms: (STARTED_AT + 45) * 1_000,
            approval_id: Some("approval-1".to_string()),
            environment_id: None,
            reason: Some(REASON.to_string()),
            network_approval_context: None,
            command: Some(APPROVAL.to_string()),
            cwd: None,
            command_actions: None,
            additional_permissions: None,
            proposed_execpolicy_amendment: None,
            proposed_network_policy_amendments: None,
            available_decisions: None,
        },
    }
}

const PROMPT: &str = "cargo test fails on the parser branch. Find out why and fix it.";

const COMMAND: &str = "cargo test -p parser";

const FAILING: &str = "test lexer::numbers ... \u{1b}[32mok\u{1b}[0m
test parser::precedence ... \u{1b}[31mFAILED\u{1b}[0m
\u{1b}[33mwarning\u{1b}[0m: unused variable: `span`
";

const PASSING: &str = "test parser::precedence ... \u{1b}[32mok\u{1b}[0m
test result: \u{1b}[32mok\u{1b}[0m. 3 passed; 0 failed
";

const PATCH: &str = "@@ -12,6 +12,6 @@ fn precedence(op: Op) -> u8 {
     match op {
         Op::Mul | Op::Div => 20,
-        Op::Neg => 30,
+        Op::Neg => 15,
         _ => 10,
     }
 }
";

const SUBAGENT: &str =
    "Checked the binding powers against the grammar: only unary minus was out of order.";

const WARNING: &str = "Unused variable `span` in src/lexer.rs; left as is.";

const ANSWER: &str = "## Fixed: operator precedence

The parser bound unary minus *tighter* than `*`, so `2 * -3 - 1` parsed as `2 * (-3 - 1)`.

| Operator | Before | After |
| --- | ---: | ---: |
| `*` `/` | 20 | 20 |
| unary `-` | 30 | 15 |

- Lowered `Op::Neg` below multiplication in `precedence()`
- See [Pratt parsing](https://example.com/pratt) for binding powers

```rust
const UNARY: u8 = 15;

#[inline]
fn precedence(op: Op) -> u8 {
    match op {
        Op::Mul | Op::Div => 20, // binds tighter
        Op::Neg => UNARY,
        _ => 10,
    }
}
```
";

const REASONING: &str =
    "- The rerun passes, so the answer is the one-line change to `precedence()`.
- `Op::Neg` drops from 30 to 15, below `*`; the `span` warning is unrelated.
";

const DRAFT: &str = "Now run the whole workspace";

const APPROVAL: &str = "cargo test --workspace";

const REASON: &str = "Every crate's tests, to be sure the fix broke nothing else.";

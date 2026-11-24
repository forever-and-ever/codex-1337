use super::process::OutputDrainPolicy;
use super::*;
use crate::codex_thread::BackgroundTerminalInfo;
use crate::exec::ExecCapturePolicy;
use crate::exec::ExecExpiration;
use crate::sandboxing::ExecRequest;
use crate::sandboxing::SandboxPermissions;
use crate::session::session::Session;
use crate::session::tests::make_session_and_context;
use crate::session::turn_context::TurnContext;
use crate::tools::context::ExecCommandToolOutput;
use crate::unified_exec::WriteStdinRequest;
use crate::unified_exec::output::CapturedStream;
use codex_exec_server::ExecProcess;
use codex_exec_server::ExecProcessEventReceiver;
use codex_exec_server::ExecProcessFuture;
use codex_exec_server::ProcessId;
use codex_exec_server::ProcessSignal;
use codex_exec_server::ReadResponse;
use codex_exec_server::StartedExecProcess;
use codex_exec_server::WriteResponse;
use codex_exec_server::WriteStatus;
use codex_sandboxing::SandboxType;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::approx_tokens_from_byte_count;
use core_test_support::skip_if_no_remote_env;
use core_test_support::skip_if_sandbox;
use core_test_support::test_codex::test_env as remote_test_env;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::sync::watch;
use tokio::time::Duration;
use tokio::time::Instant;

async fn test_session_and_turn() -> (Arc<Session>, Arc<TurnContext>) {
    let (session, turn) = make_session_and_context().await;
    (Arc::new(session), Arc::new(turn))
}

async fn exec_command(
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    cmd: &str,
    yield_time_ms: u64,
    workdir: Option<PathBuf>,
) -> Result<ExecCommandToolOutput, UnifiedExecError> {
    exec_command_with_tty(
        session,
        turn,
        cmd,
        yield_time_ms,
        workdir,
        /*tty*/ true,
    )
    .await
}

fn shell_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

fn test_exec_request(
    turn: &TurnContext,
    command: Vec<String>,
    cwd: AbsolutePathBuf,
    env: HashMap<String, String>,
) -> ExecRequest {
    let permission_profile = turn.permission_profile();
    let arg0 = None;
    ExecRequest::new(
        command,
        cwd,
        env,
        ExecExpiration::DefaultTimeout,
        ExecCapturePolicy::ShellTool,
        SandboxType::None,
        turn.config
            .effective_workspace_roots()
            .iter()
            .map(PathUri::to_abs_path)
            .collect::<std::io::Result<Vec<_>>>()
            .expect("test workspace roots are host-native"),
        turn.windows_sandbox_level,
        permission_profile,
        arg0,
    )
}

async fn exec_command_with_tty(
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    cmd: &str,
    yield_time_ms: u64,
    workdir: Option<PathBuf>,
    tty: bool,
) -> Result<ExecCommandToolOutput, UnifiedExecError> {
    let manager = &session.services.unified_exec_manager;
    let process_id = manager.allocate_process_id().await;
    #[allow(deprecated)]
    let cwd = workdir
        .as_ref()
        .map_or_else(|| turn.cwd.clone(), |workdir| turn.cwd.join(workdir));
    let command = vec!["bash".to_string(), "-lc".to_string(), cmd.to_string()];
    let request = test_exec_request(turn, command.clone(), cwd.clone(), shell_env());

    let process = manager
        .open_session_with_prepared_exec_env(
            process_id,
            &request,
            /*tool_ctx*/ None,
            codex_sandboxing::WindowsSandboxProxySettingsMode::Reconcile,
            /*network_policy_decider*/ None,
            tty,
            turn.initial_environments
                .primary()
                .expect("turn environment")
                .environment()
                .as_ref(),
        )
        .await?;
    let context = UnifiedExecContext::new(
        Arc::clone(session),
        crate::session::step_context::StepContext::for_test(Arc::clone(turn)),
        tokio_util::sync::CancellationToken::new(),
        "call".to_string(),
        codex_tools::ToolName::plain("exec_command"),
    );
    let started_at = Instant::now();
    let process_started_alive = !process.has_exited() && process.exit_code().is_none();
    if process_started_alive {
        let entry = ProcessEntry {
            process: Arc::clone(&process),
            plugin_metrics_sidecar: None,
            call_id: context.call_id.clone(),
            process_id,
            cwd: cwd.clone().into(),
            initial_exec_command_active: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            hook_context: crate::tools::context::ExecHookContext {
                tool_name: codex_tools::ToolName::namespaced("functions", "exec_command"),
                command: cmd.to_string(),
            },
            tty,
            environment_id: codex_exec_server::LOCAL_ENVIRONMENT_ID.to_string(),
            permissions: TerminalPermissions::for_launch(
                turn.initial_environments
                    .primary()
                    .expect("turn environment"),
                turn,
                TerminalSandboxSource::Native,
                SandboxPermissions::UseDefault,
                /*additional_permissions*/ None,
                /*internal_permissions*/ None,
            ),
            network_approval: None,
            session: Arc::downgrade(session),
            last_used: started_at,
        };
        manager
            .process_store
            .lock()
            .await
            .processes
            .insert(process_id, entry);
    }

    let deadline = started_at + Duration::from_millis(yield_time_ms);
    let collected_output = super::output_collection::collect(
        process.output_handles(),
        Some(session.subscribe_elicitation_pause_state()),
        deadline,
        /*interjection*/ None,
    )
    .await;
    let wall_time = Instant::now().saturating_duration_since(started_at);
    let original_token_count = usize::try_from(approx_tokens_from_byte_count(
        collected_output.total_bytes(),
    ))
    .unwrap_or(usize::MAX);
    let collected = CapturedStream::from(&collected_output);
    let has_exited = process.has_exited();
    let exit_code = process.exit_code();
    let response_process_id = if process_started_alive && !has_exited {
        Some(process_id)
    } else {
        manager.release_process_id(process_id).await;
        None
    };
    if response_process_id.is_some()
        && let Some(entry) = manager
            .process_store
            .lock()
            .await
            .processes
            .get_mut(&process_id)
    {
        entry
            .initial_exec_command_active
            .store(false, std::sync::atomic::Ordering::Release);
    }

    Ok(ExecCommandToolOutput {
        event_call_id: context.call_id,
        chunk_id: generate_chunk_id(),
        wall_time,
        raw_output: collected,
        truncation_policy: turn.model_info().truncation_policy.into(),
        max_output_tokens: None,
        process_id: response_process_id,
        exit_code,
        original_token_count: Some(original_token_count),
        hook_context: Some(crate::tools::context::ExecHookContext {
            tool_name: codex_tools::ToolName::plain("exec_command"),
            command: cmd.to_string(),
        }),
        tool_cwd: None,
    })
}

struct BlockingTerminateExecProcess {
    process_id: ProcessId,
    terminate_started: watch::Sender<bool>,
    allow_terminate: Arc<Notify>,
    wake_tx: watch::Sender<u64>,
}

impl BlockingTerminateExecProcess {
    async fn read(&self) -> Result<ReadResponse, codex_exec_server::ExecServerError> {
        Ok(ReadResponse {
            chunks: Vec::new(),
            next_seq: 1,
            exited: false,
            exit_code: None,
            closed: false,
            failure: None,
            sandbox_denied: false,
        })
    }

    async fn write(&self) -> Result<WriteResponse, codex_exec_server::ExecServerError> {
        Ok(WriteResponse {
            status: WriteStatus::Accepted,
        })
    }

    async fn terminate(&self) -> Result<(), codex_exec_server::ExecServerError> {
        let _ = self.terminate_started.send(true);
        self.allow_terminate.notified().await;
        Ok(())
    }
}

impl ExecProcess for BlockingTerminateExecProcess {
    fn process_id(&self) -> &ProcessId {
        &self.process_id
    }

    fn subscribe_wake(&self) -> watch::Receiver<u64> {
        self.wake_tx.subscribe()
    }

    fn subscribe_events(&self) -> ExecProcessEventReceiver {
        ExecProcessEventReceiver::empty()
    }

    fn read(
        &self,
        _after_seq: Option<u64>,
        _max_bytes: Option<usize>,
        _wait_ms: Option<u64>,
    ) -> ExecProcessFuture<'_, ReadResponse> {
        Box::pin(BlockingTerminateExecProcess::read(self))
    }

    fn write(&self, _chunk: Vec<u8>) -> ExecProcessFuture<'_, WriteResponse> {
        Box::pin(BlockingTerminateExecProcess::write(self))
    }

    fn signal(&self, _signal: ProcessSignal) -> ExecProcessFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn terminate(&self) -> ExecProcessFuture<'_, ()> {
        Box::pin(BlockingTerminateExecProcess::terminate(self))
    }
}

async fn blocking_terminate_unified_process(
    process_id: i32,
    terminate_started: watch::Sender<bool>,
    allow_terminate: Arc<Notify>,
) -> anyhow::Result<Arc<UnifiedExecProcess>> {
    let (wake_tx, _wake_rx) = watch::channel(0);
    Ok(UnifiedExecProcess::from_exec_server_started(
        StartedExecProcess {
            process: Arc::new(BlockingTerminateExecProcess {
                process_id: process_id.to_string().into(),
                terminate_started,
                allow_terminate,
                wake_tx,
            }),
            sandbox_type: Some(codex_sandboxing::SandboxType::None),
        },
        OutputDrainPolicy::WaitForOutputClosure,
        |_| {},
    )
    .await?)
}

async fn write_stdin(
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    process_id: i32,
    input: &str,
    yield_time_ms: u64,
) -> Result<ExecCommandToolOutput, UnifiedExecError> {
    let manager = &session.services.unified_exec_manager;
    let terminal = manager.resolve_terminal(process_id).await?;
    manager
        .write_stdin(
            &UnifiedExecContext::new(
                Arc::clone(session),
                crate::session::step_context::StepContext::for_test(Arc::clone(turn)),
                tokio_util::sync::CancellationToken::new(),
                "write".to_string(),
                codex_tools::ToolName::plain("write_stdin"),
            ),
            WriteStdinRequest {
                terminal: &terminal,
                input,
                yield_time_ms,
                max_output_tokens: None,
                truncation_policy: TruncationPolicy::Tokens(10_000),
                interaction_event: None,
            },
        )
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unified_exec_persists_across_requests() -> anyhow::Result<()> {
    skip_if_sandbox!(Ok(()));

    let (session, turn) = test_session_and_turn().await;
    #[allow(deprecated)]
    let cwd = turn.cwd.clone();

    let open_shell = exec_command(
        &session, &turn, "bash -i", /*yield_time_ms*/ 2_500, /*workdir*/ None,
    )
    .await?;
    let process_id = open_shell.process_id.expect("expected process_id");
    assert_eq!(
        session.list_background_terminals().await,
        vec![BackgroundTerminalInfo {
            item_id: "call".to_string(),
            process_id: process_id.to_string(),
            command: "bash -i".to_string(),
            cwd: cwd.into(),
        }]
    );

    write_stdin(
        &session,
        &turn,
        process_id,
        "export CODEX_INTERACTIVE_SHELL_VAR=codex\n",
        /*yield_time_ms*/ 2_500,
    )
    .await?;

    let out_2 = write_stdin(
        &session,
        &turn,
        process_id,
        "echo $CODEX_INTERACTIVE_SHELL_VAR\n",
        /*yield_time_ms*/ 2_500,
    )
    .await?;
    assert!(
        out_2
            .truncated_output(DEFAULT_MAX_OUTPUT_TOKENS)
            .contains("codex"),
        "expected environment variable output"
    );

    assert!(session.terminate_background_terminal(process_id).await);
    assert!(!session.terminate_background_terminal(process_id).await);
    assert!(session.list_background_terminals().await.is_empty());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_unified_exec_sessions() -> anyhow::Result<()> {
    skip_if_sandbox!(Ok(()));

    let (session, turn) = test_session_and_turn().await;

    let shell_a = exec_command(
        &session, &turn, "bash -i", /*yield_time_ms*/ 2_500, /*workdir*/ None,
    )
    .await?;
    let session_a = shell_a.process_id.expect("expected process id");

    write_stdin(
        &session,
        &turn,
        session_a,
        "export CODEX_INTERACTIVE_SHELL_VAR=codex\n",
        /*yield_time_ms*/ 2_500,
    )
    .await?;

    let out_2 = exec_command(
        &session,
        &turn,
        "echo $CODEX_INTERACTIVE_SHELL_VAR",
        /*yield_time_ms*/ 2_500,
        /*workdir*/ None,
    )
    .await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        out_2.process_id.is_none(),
        "short command should not report a process id if it exits quickly"
    );
    assert!(
        !out_2
            .truncated_output(DEFAULT_MAX_OUTPUT_TOKENS)
            .contains("codex"),
        "short command should run in a fresh shell"
    );

    let out_3 = write_stdin(
        &session,
        &turn,
        shell_a.process_id.expect("expected process id"),
        "echo $CODEX_INTERACTIVE_SHELL_VAR\n",
        /*yield_time_ms*/ 2_500,
    )
    .await?;
    assert!(
        out_3
            .truncated_output(DEFAULT_MAX_OUTPUT_TOKENS)
            .contains("codex"),
        "session should preserve state"
    );

    Ok(())
}

#[tokio::test]
async fn unified_exec_timeouts() -> anyhow::Result<()> {
    skip_if_sandbox!(Ok(()));

    const TEST_VAR_VALUE: &str = "unified_exec_var_123";
    let (session, turn) = test_session_and_turn().await;
    let open_shell = exec_command(
        &session,
        &turn,
        &format!("CODEX_INTERACTIVE_SHELL_VAR={TEST_VAR_VALUE} bash -i"),
        /*yield_time_ms*/ 2_500,
        /*workdir*/ None,
    )
    .await?;
    let process_id = open_shell.process_id.expect("expected process id");

    // The shell cannot emit the value until the next write releases its read.
    let blocked = write_stdin(
        &session,
        &turn,
        process_id,
        "read -r release && echo \"$CODEX_INTERACTIVE_SHELL_VAR\"\n",
        /*yield_time_ms*/ 10,
    )
    .await?;
    assert_eq!(blocked.process_id, Some(process_id));
    assert!(
        !blocked
            .truncated_output(DEFAULT_MAX_OUTPUT_TOKENS)
            .contains(TEST_VAR_VALUE),
        "short collection must yield while the shell is blocked"
    );

    // Empty polls deliberately wait at least a minute; release and close the fixture instead.
    let released = write_stdin(
        &session,
        &turn,
        process_id,
        "release\nexit\n",
        /*yield_time_ms*/ 1_000,
    )
    .await?;
    assert!(
        released
            .truncated_output(DEFAULT_MAX_OUTPUT_TOKENS)
            .contains(TEST_VAR_VALUE),
        "the next collection should retrieve the released output"
    );
    assert_eq!(released.exit_code, Some(0));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unified_exec_pause_blocks_yield_timeout() -> anyhow::Result<()> {
    skip_if_sandbox!(Ok(()));

    let (session, turn) = test_session_and_turn().await;
    let elicitation = session.services.elicitations.register();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        drop(elicitation);
    });

    let started = tokio::time::Instant::now();
    let response = exec_command(
        &session,
        &turn,
        "sleep 1 && echo unified-exec-done",
        /*yield_time_ms*/ 250,
        /*workdir*/ None,
    )
    .await?;

    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "pause should block the unified exec yield timeout"
    );
    assert!(
        response
            .truncated_output(DEFAULT_MAX_OUTPUT_TOKENS)
            .contains("unified-exec-done"),
        "exec_command should wait for output after the pause lifts"
    );
    assert!(
        response.process_id.is_none(),
        "completed command should not leave a background process"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_process_receipts_preserve_outcome_without_stdin_authority() -> anyhow::Result<()>
{
    skip_if_sandbox!(Ok(()));

    let (session, turn) = test_session_and_turn().await;
    let manager = &session.services.unified_exec_manager;
    let open_shell = exec_command(
        &session, &turn, "bash -i", /*yield_time_ms*/ 250, /*workdir*/ None,
    )
    .await?;
    let process_id = open_shell.process_id.expect("expected process id");
    let launched = manager.resolve_terminal(process_id).await?;
    let original_environment_id = launched.environment_id.clone();
    let process = Arc::clone(launched.process().expect("live terminal"));
    let waiting = write_stdin(
        &session,
        &turn,
        process_id,
        "printf '%20000s' ''; printf 'completion-tail\\n'; read -r gate; exit 17\n",
        /*yield_time_ms*/ 250,
    )
    .await?;
    assert_eq!(waiting.process_id, Some(process_id));
    let release = async {
        while process.interaction_lock().try_lock_owned().is_ok() {
            tokio::task::yield_now().await;
        }
        process.write(b"release\n").await
    };
    let (completed, queued, released) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            write_stdin(&session, &turn, process_id, "", 3_600_000),
            write_stdin(&session, &turn, process_id, "", 100),
            release
        )
    })
    .await?;
    released?;
    let completed = completed?;
    assert_eq!(completed.exit_code, Some(17));
    let queued = queued?;
    assert_eq!(queued.exit_code, completed.exit_code);
    assert!(queued.hook_context.is_none());

    // Finalization removes stdin authority, not completion evidence. Concurrent observers
    // must see the same outcome, and replay must not re-emit completion hooks.
    let (first, second) = tokio::join!(
        write_stdin(&session, &turn, process_id, "", 3_600_000),
        write_stdin(&session, &turn, process_id, "", 100)
    );
    for receipt in [first?, second?] {
        assert_eq!(receipt.exit_code, Some(17));
        assert_eq!(receipt.process_id, None);
        assert!(receipt.raw_output.text.contains("completion-tail"));
        assert!(receipt.raw_output.text.len() < 17 * 1024);
        assert!(receipt.raw_output.text.starts_with("... "));
        assert!(receipt.raw_output.omitted_bytes > 0);
        assert!(receipt.hook_context.is_none());
    }
    assert!(matches!(
        write_stdin(&session, &turn, process_id, "echo never replayed\n", 250).await,
        Err(UnifiedExecError::UnknownProcessId { process_id: id }) if id == process_id
    ));
    assert!(manager.process_store.lock().await.processes.is_empty());

    let completed_terminal = manager.resolve_terminal(process_id).await?;
    assert!(completed_terminal.process().is_none());
    assert_eq!(completed_terminal.environment_id, original_environment_id);
    assert_eq!(completed_terminal.call_id, completed.event_call_id);
    let (foreign, foreign_turn) = test_session_and_turn().await;
    let foreign_context = UnifiedExecContext::new(
        foreign,
        crate::session::step_context::StepContext::for_test(foreign_turn),
        tokio_util::sync::CancellationToken::new(),
        "foreign".into(),
        codex_tools::ToolName::plain("write_stdin"),
    );
    assert!(
        manager
            .completed_poll(
                &foreign_context,
                &WriteStdinRequest {
                    terminal: &completed_terminal,
                    input: "",
                    yield_time_ms: 250,
                    max_output_tokens: None,
                    truncation_policy: TruncationPolicy::Tokens(10_000),
                    interaction_event: None,
                }
            )
            .await
            .is_err()
    );

    // Evicting evidence cannot turn a stale session ID into authority over a new process.
    let context = UnifiedExecContext::new(
        Arc::clone(&session),
        crate::session::step_context::StepContext::for_test(Arc::clone(&turn)),
        tokio_util::sync::CancellationToken::new(),
        "receipt".into(),
        codex_tools::ToolName::plain("write_stdin"),
    );
    for _ in 0..64 {
        let id = manager.allocate_process_id().await;
        manager
            .retain_completion(
                id,
                &completed_terminal.environment_id,
                &process,
                &context,
                &completed,
            )
            .await;
    }
    assert!(matches!(
        write_stdin(&session, &turn, process_id, "", 250).await,
        Err(UnifiedExecError::UnknownProcessId { process_id: id }) if id == process_id
    ));
    assert!(manager.allocate_process_id().await > process_id + 64);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminating_initial_exec_command_rechecks_initial_response_state() -> anyhow::Result<()> {
    let (session, turn) = test_session_and_turn().await;
    let manager = &session.services.unified_exec_manager;
    let process_id = manager.allocate_process_id().await;
    let (terminate_started_tx, mut terminate_started_rx) = watch::channel(false);
    let allow_terminate = Arc::new(Notify::new());
    let process = blocking_terminate_unified_process(
        process_id,
        terminate_started_tx,
        Arc::clone(&allow_terminate),
    )
    .await?;
    #[allow(deprecated)]
    let cwd = turn.cwd.clone();
    manager.process_store.lock().await.processes.insert(
        process_id,
        ProcessEntry {
            process,
            plugin_metrics_sidecar: None,
            call_id: "call".to_string(),
            process_id,
            cwd: cwd.into(),
            initial_exec_command_active: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            hook_context: crate::tools::context::ExecHookContext {
                tool_name: codex_tools::ToolName::namespaced("functions", "exec_command"),
                command: "sleep 60".to_string(),
            },
            tty: true,
            environment_id: codex_exec_server::LOCAL_ENVIRONMENT_ID.to_string(),
            permissions: TerminalPermissions::for_launch(
                turn.initial_environments
                    .primary()
                    .expect("turn environment"),
                &turn,
                TerminalSandboxSource::Native,
                SandboxPermissions::UseDefault,
                /*additional_permissions*/ None,
                /*internal_permissions*/ None,
            ),
            network_approval: None,
            session: Arc::downgrade(&session),
            last_used: Instant::now(),
        },
    );

    let terminate_task = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.terminate_background_terminal(process_id).await }
    });
    tokio::time::timeout(
        Duration::from_secs(2),
        terminate_started_rx.wait_for(|started| *started),
    )
    .await
    .expect("terminate should start")
    .expect("terminate signal sender should stay open");

    {
        let mut store = manager.process_store.lock().await;
        let entry = store
            .processes
            .get_mut(&process_id)
            .expect("process should remain stored until initial response returns");
        entry
            .initial_exec_command_active
            .store(false, std::sync::atomic::Ordering::Release);
    }

    // Retain the release even if the sole termination waiter has not registered yet.
    allow_terminate.notify_one();
    let terminated = tokio::time::timeout(Duration::from_secs(2), terminate_task)
        .await
        .expect("terminate should finish")
        .expect("terminate task should not panic");
    assert!(terminated);
    assert!(
        !manager
            .process_store
            .lock()
            .await
            .processes
            .contains_key(&process_id)
    );

    Ok(())
}

#[test_case::test_case(false; "terminal removed")]
#[test_case::test_case(true; "session id reused")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminating_during_stdin_poll_returns_exited_response(
    reuse_id: bool,
) -> anyhow::Result<()> {
    let (session, turn) = test_session_and_turn().await;
    let manager = &session.services.unified_exec_manager;
    let process_id = manager.allocate_process_id().await;
    let (terminate_started_tx, _terminate_started_rx) = watch::channel(false);
    let allow_terminate = Arc::new(Notify::new());
    let process = blocking_terminate_unified_process(
        process_id,
        terminate_started_tx,
        Arc::clone(&allow_terminate),
    )
    .await?;
    #[allow(deprecated)]
    let cwd = turn.cwd.clone();
    let last_used = Instant::now() - Duration::from_secs(1);
    manager.process_store.lock().await.processes.insert(
        process_id,
        ProcessEntry {
            process: Arc::clone(&process),
            plugin_metrics_sidecar: None,
            call_id: "call".to_string(),
            process_id,
            cwd: cwd.into(),
            initial_exec_command_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hook_context: crate::tools::context::ExecHookContext {
                tool_name: codex_tools::ToolName::namespaced("functions", "exec_command"),
                command: "sleep 60".to_string(),
            },
            tty: true,
            environment_id: codex_exec_server::LOCAL_ENVIRONMENT_ID.to_string(),
            permissions: TerminalPermissions::for_launch(
                turn.initial_environments
                    .primary()
                    .expect("turn environment"),
                &turn,
                TerminalSandboxSource::Native,
                SandboxPermissions::UseDefault,
                /*additional_permissions*/ None,
                /*internal_permissions*/ None,
            ),
            network_approval: None,
            session: Arc::downgrade(&session),
            last_used,
        },
    );

    let poll_task = tokio::spawn({
        let session = Arc::clone(&session);
        let turn = Arc::clone(&turn);
        async move {
            write_stdin(
                &session, &turn, process_id, "", /*yield_time_ms*/ 60_000,
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let poll_started = manager
                .process_store
                .lock()
                .await
                .processes
                .get(&process_id)
                .is_some_and(|entry| entry.last_used != last_used);
            if poll_started {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("poll should clone process handles");

    if reuse_id {
        let replacement = super::process_tests::remote_process(
            WriteStatus::Accepted,
            /*terminate_error*/ None,
            SandboxType::None,
        )
        .await;
        let mut store = manager.process_store.lock().await;
        let entry = store.processes.get_mut(&process_id).unwrap();
        entry.process = replacement;
        entry.call_id = "replacement".into();
    } else {
        manager.release_process_id(process_id).await;
    }
    allow_terminate.notify_one();
    process.terminate_confirmed().await?;

    let output = tokio::time::timeout(Duration::from_secs(2), poll_task)
        .await
        .expect("poll should finish")
        .expect("poll task should not panic")?;
    assert_eq!(
        (output.process_id, output.event_call_id.as_str()),
        (None, "call")
    );
    assert_eq!(
        manager
            .process_store
            .lock()
            .await
            .processes
            .contains_key(&process_id),
        reuse_id
    );

    Ok(())
}

#[test_case::test_case(false; "pipe")]
#[cfg_attr(target_os = "linux", test_case::test_case(true; "PTY with non-UTF-8 sandbox launcher"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_local_commands_preserve_exit_code(tty: bool) -> anyhow::Result<()> {
    let (_, turn) = make_session_and_context().await;
    // Classification must distinguish repository prose from a denial after a
    // prepared child exits; neither case may lose its nonzero exit or output.
    for (sandbox, text, denied) in [
        (SandboxType::None, "", false),
        (
            SandboxType::LinuxSeccomp,
            "sandbox seccomp landlock: failed to write file: No space left on device",
            false,
        ),
        (
            SandboxType::LinuxSeccomp,
            "cannot create file: Permission denied",
            true,
        ),
    ] {
        #[allow(deprecated)]
        let cwd = turn.cwd.clone();
        let mut env = shell_env();
        env.insert("COMPLETED_OUTPUT".to_string(), text.to_string());
        let cwd = PathUri::from_abs_path(&cwd);
        let command = vec![
            "bash".to_string(),
            "-lc".to_string(),
            "printf '%s' \"$COMPLETED_OUTPUT\"; exit 17".to_string(),
        ];
        let native_program = None;
        #[cfg(target_os = "linux")]
        let (command, native_program, _helper_dir) = if tty {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir()?;
            let helper = dir.path().join(OsStr::from_bytes(b"helper-\xff"));
            std::fs::write(
                &helper,
                "#!/bin/sh\nprintf '%s' \"$COMPLETED_OUTPUT\"; exit 17\n",
            )?;
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
            std::os::unix::fs::symlink("/bin/false", helper.to_string_lossy().as_ref())?;
            (
                vec![helper.to_string_lossy().into_owned()],
                Some(helper.into_os_string()),
                Some(dir),
            )
        } else {
            (command, native_program, None)
        };
        // The prepared-launch boundary owns classification, not policy setup.
        let request = ExecRequest::from_sandbox_exec_request(
            codex_sandboxing::SandboxExecRequest {
                command,
                native_program,
                cwd: cwd.clone(),
                sandbox_policy_cwd: cwd,
                env,
                network: None,
                network_environment_id: None,
                sandbox,
                sandbox_override: codex_protocol::sandbox::SandboxOverride::NoOverride,
                windows_sandbox_level: turn.windows_sandbox_level,
                permission_profile: turn.permission_profile(),
                arg0: None,
            },
            crate::sandboxing::ExecOptions {
                expiration: ExecExpiration::DefaultTimeout,
                capture_policy: ExecCapturePolicy::ShellTool,
            },
            &[],
        )
        .await?;
        let environment = codex_exec_server::Environment::default_for_tests();
        let result = UnifiedExecProcessManager::default()
            .open_session_with_prepared_exec_env(
                /*process_id*/ 1234,
                &request,
                /*tool_ctx*/ None,
                codex_sandboxing::WindowsSandboxProxySettingsMode::Reconcile,
                /*network_policy_decider*/ None,
                tty,
                &environment,
            )
            .await;

        let denial = match result {
            Err(err) => Some(err),
            Ok(process) => {
                if !process.has_exited() {
                    let exit_signal = process.cancellation_token();
                    assert!(
                        tokio::time::timeout(Duration::from_secs(2), exit_signal.cancelled())
                            .await
                            .is_ok(),
                        "process did not report exit within timeout"
                    );
                }
                assert!(process.has_exited());
                assert_eq!(process.exit_code(), Some(17));
                let denial = process.check_for_sandbox_denial().await.err();
                let output = process
                    .output_handles()
                    .output_buffer
                    .lock()
                    .await
                    .pending
                    .to_bytes();
                assert_eq!(String::from_utf8_lossy(&output), text);
                denial
            }
        };
        match denial {
            Some(UnifiedExecError::SandboxDenied { output, .. }) => {
                assert!(denied, "repository prose is not denial evidence");
                assert_eq!(output.exit_code, 17);
                assert_eq!(output.aggregated_output.text, text);
            }
            Some(err) => return Err(err.into()),
            None => assert!(!denied, "expected sandbox denial"),
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unified_exec_uses_remote_exec_server_when_configured() -> anyhow::Result<()> {
    skip_if_sandbox!(Ok(()));
    skip_if_no_remote_env!(Ok(()));

    let remote_test_env = remote_test_env().await?;
    let (_, turn) = make_session_and_context().await;
    let request = test_exec_request(
        &turn,
        vec!["bash".to_string(), "-i".to_string()],
        remote_test_env.cwd().clone(),
        shell_env(),
    );

    let manager = UnifiedExecProcessManager::default();
    let process = manager
        .open_session_with_prepared_exec_env(
            /*process_id*/ 1234,
            &request,
            /*tool_ctx*/ None,
            codex_sandboxing::WindowsSandboxProxySettingsMode::Reconcile,
            /*network_policy_decider*/ None,
            /*tty*/ true,
            remote_test_env.environment(),
        )
        .await?;

    process.write(b"printf 'remote-unified-exec\\n'\n").await?;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let collected = super::output_collection::collect(
        process.output_handles(),
        /*pause_state*/ None,
        Instant::now() + Duration::from_millis(2_500),
        /*interjection*/ None,
    )
    .await
    .to_bytes_with_omission_marker();

    assert!(String::from_utf8_lossy(&collected).contains("remote-unified-exec"));
    Ok(())
}

#[tokio::test]
async fn stdin_approval_preserves_the_reviewed_terminal() -> anyhow::Result<()> {
    use crate::session::tests::make_session_and_context_with_auth_and_config_and_rx;
    use crate::state::ActiveTurn;
    use crate::tools::sandboxing::ToolError;
    use codex_features::Feature;
    use codex_protocol::config_types::ApprovalsReviewer;
    use codex_protocol::protocol::AskForApproval;
    use codex_protocol::protocol::EventMsg;
    use codex_protocol::protocol::ReviewDecision;

    skip_if_sandbox!(Ok(()));
    let (session, mut turn, events) = make_session_and_context_with_auth_and_config_and_rx(
        codex_login::CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config.features.enable(Feature::WriteStdinApproval).unwrap();
            config.permissions.approval_policy =
                crate::config::Constrained::allow_any(AskForApproval::OnRequest);
            config.approvals_reviewer = ApprovalsReviewer::AutoReview;
        },
    )
    .await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());
    let manager = &session.services.unified_exec_manager;
    let command = "while IFS= read -r line; do printf 'received:%s\\n' \"$line\"; done";
    let opened = exec_command(
        &session, &turn, command, /*yield_time_ms*/ 250, /*workdir*/ None,
    )
    .await?;
    let process_id = opened.process_id.expect("running terminal");
    let cwd = PathUri::parse("file:///C:/workspace")?;
    {
        let mut store = manager.process_store.lock().await;
        let entry = store.processes.get_mut(&process_id).unwrap();
        entry.permissions = TerminalPermissions::for_launch(
            turn.initial_environments
                .primary()
                .expect("turn environment"),
            &turn,
            TerminalSandboxSource::Native,
            SandboxPermissions::RequireEscalated,
            /*additional_permissions*/ None,
            /*internal_permissions*/ None,
        );
        entry.environment_id = "unselected-executor".to_string();
        entry.cwd = cwd.clone();
    }
    let original = manager.resolve_terminal(process_id).await?;
    let original_process = Arc::clone(original.process().expect("live terminal"));
    assert_eq!(original.environment_id, "unselected-executor");
    // Only collection consumes this wake, after approval selection; a spurious
    // approval still fails without waiting for the production empty-poll minimum.
    {
        let poll_interjection = tokio_util::sync::CancellationToken::new();
        poll_interjection.cancel();
        let mut poll_context = UnifiedExecContext::new(
            Arc::clone(&session),
            crate::session::step_context::StepContext::for_test(Arc::clone(&turn)),
            tokio_util::sync::CancellationToken::new(),
            "write".to_string(),
            codex_tools::ToolName::plain("write_stdin"),
        );
        poll_context.poll_interjection = Some(poll_interjection);
        let polled = tokio::time::timeout(
            Duration::from_secs(/*secs*/ 5),
            manager.write_stdin(
                &poll_context,
                WriteStdinRequest {
                    terminal: &original,
                    input: "",
                    yield_time_ms: 250,
                    max_output_tokens: None,
                    truncation_policy: TruncationPolicy::Tokens(10_000),
                    interaction_event: None,
                },
            ),
        )
        .await??;
        assert_eq!(polled.process_id, Some(process_id));
        assert!(!original_process.has_exited());
        assert!(!poll_context.cancellation_token.is_cancelled());
    }
    let input = "rejected\n";
    let denied = write_stdin(
        &session, &turn, process_id, input, /*yield_time_ms*/ 250,
    )
    .await;
    assert!(
        matches!(denied, Err(UnifiedExecError::StdinApproval(ToolError::Rejected(reason)))
        if reason.contains("select it before retrying"))
    );
    Arc::make_mut(&mut Arc::get_mut(&mut turn).unwrap().config).approvals_reviewer =
        ApprovalsReviewer::User;
    assert!(matches!(
        write_stdin(&session, &turn, process_id, input, /*yield_time_ms*/ 250).await,
        Err(UnifiedExecError::StdinApproval(ToolError::Rejected(reason)))
            if reason.contains("select it before retrying")
    ));
    manager
        .process_store
        .lock()
        .await
        .processes
        .get_mut(&process_id)
        .unwrap()
        .environment_id = turn
        .initial_environments
        .primary()
        .unwrap()
        .selection
        .environment_id
        .clone();
    for (input, decision) in [
        ("rejected\n", ReviewDecision::denied("test denial")),
        ("accepted\n", ReviewDecision::Approved),
        ("replace\n", ReviewDecision::Approved),
    ] {
        let review = async {
            loop {
                let EventMsg::ExecApprovalRequest(approval) = events.recv().await?.msg else {
                    continue;
                };
                assert_eq!(
                    (
                        approval.call_id.as_str(),
                        approval.approval_id.as_deref(),
                        approval.cwd
                    ),
                    ("call", Some("write"), cwd.clone().into())
                );
                assert!(
                    original_process
                        .interaction_lock()
                        .try_lock_owned()
                        .is_err()
                );
                if input == "replace\n" {
                    let replacement = super::process_tests::remote_process(
                        WriteStatus::Accepted,
                        /*terminate_error*/ None,
                        SandboxType::None,
                    )
                    .await;
                    let mut store = manager.process_store.lock().await;
                    store.processes.get_mut(&process_id).unwrap().process = replacement;
                }
                session.notify_approval("write", decision).await;
                return anyhow::Ok(());
            }
        };
        let (result, reviewed) = tokio::time::timeout(Duration::from_secs(/*secs*/ 5), async {
            tokio::join!(
                write_stdin(
                    &session, &turn, process_id, input, /*yield_time_ms*/ 250
                ),
                review
            )
        })
        .await?;
        reviewed?;
        match input {
            "rejected\n" => assert!(matches!(result, Err(UnifiedExecError::StdinApproval(_)))),
            "accepted\n" => {
                let output = result?.truncated_output(DEFAULT_MAX_OUTPUT_TOKENS);
                assert!(output.contains("received:accepted"), "{output}");
                assert!(!output.contains("received:rejected"), "{output}");
            }
            _ => assert!(
                matches!(result, Err(UnifiedExecError::UnknownProcessId { process_id: id }) if id == process_id)
            ),
        }
        assert!(original_process.interaction_lock().try_lock_owned().is_ok());
    }
    // Reusing the numeric ID must also reject a subsequent write to the original terminal.
    let context = UnifiedExecContext::new(
        Arc::clone(&session),
        crate::session::step_context::StepContext::for_test(turn),
        tokio_util::sync::CancellationToken::new(),
        "stale".to_string(),
        codex_tools::ToolName::plain("write_stdin"),
    );
    let stale = manager
        .write_stdin(
            &context,
            WriteStdinRequest {
                terminal: &original,
                input: "",
                yield_time_ms: 250,
                max_output_tokens: None,
                truncation_policy: TruncationPolicy::Tokens(10_000),
                interaction_event: None,
            },
        )
        .await;
    assert!(
        matches!(stale, Err(UnifiedExecError::UnknownProcessId { process_id: id }) if id == process_id)
    );
    original_process.terminate();
    assert!(session.terminate_background_terminal(process_id).await);
    Ok(())
}

/// A queued write observes strict review enabled on its captured step while waiting
/// for another terminal interaction to finish.
#[tokio::test]
async fn stdin_approval_observes_strict_review_enabled_while_queued() -> anyhow::Result<()> {
    use crate::session::step_context::StepContext;
    use crate::session::tests::make_session_and_context_with_auth_and_config_and_rx;
    use crate::tools::sandboxing::ToolError;
    use codex_features::Feature;

    skip_if_sandbox!(Ok(()));
    let (session, turn, _events) = make_session_and_context_with_auth_and_config_and_rx(
        codex_login::CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config.features.enable(Feature::WriteStdinApproval).unwrap();
        },
    )
    .await;
    let manager = &session.services.unified_exec_manager;
    let opened = exec_command(
        &session, &turn, "cat", /*yield_time_ms*/ 250, /*workdir*/ None,
    )
    .await?;
    let process_id = opened.process_id.expect("running terminal");
    let terminal = manager.resolve_terminal(process_id).await?;
    let context = UnifiedExecContext::new(
        Arc::clone(&session),
        StepContext::for_test(turn),
        tokio_util::sync::CancellationToken::new(),
        "write".to_string(),
        codex_tools::ToolName::plain("write_stdin"),
    );
    let process = terminal.process().expect("live terminal");
    let interaction = process.interaction_lock().lock_owned().await;
    let mut queued = Box::pin(manager.write_stdin(
        &context,
        WriteStdinRequest {
            terminal: &terminal,
            // NUL input must be rejected when strict review becomes required.
            input: "\0",
            yield_time_ms: 250,
            max_output_tokens: None,
            truncation_policy: TruncationPolicy::Tokens(10_000),
            interaction_event: None,
        },
    ));
    let mut task_context = std::task::Context::from_waker(futures::task::noop_waker_ref());
    assert!(queued.as_mut().poll(&mut task_context).is_pending());
    context.step_context.turn.record_granted_permissions(
        codex_exec_server::LOCAL_ENVIRONMENT_ID,
        Default::default(),
        /*strict_auto_review*/ true,
    );
    drop(interaction);
    assert!(matches!(
        queued.await,
        Err(UnifiedExecError::StdinApproval(ToolError::Rejected(reason)))
            if reason.contains("NUL byte")
    ));
    assert!(session.terminate_background_terminal(process_id).await);
    Ok(())
}

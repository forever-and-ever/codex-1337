use super::*;
use std::sync::mpsc;
use std::time::Duration;

// This is a control-plane failure witness, not a counter accessor test: the UI
// thread is first synchronously blocked, then suspended in an unfinished RPC.
#[tokio::test(flavor = "current_thread")]
async fn independent_dump_survives_blocked_dispatcher_and_pending_rpc() {
    let shared = Arc::new(Shared::new());
    let endpoint = endpoint::Endpoint::start(Arc::clone(&shared)).expect("listener");
    let pid = std::process::id();
    let (entered, observer) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let (rpc_done, rpc_pending) = tokio::sync::oneshot::channel();
    let reader = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("observer runtime");
        observer
            .recv_timeout(Duration::from_secs(5))
            .expect("sync stage entered");
        runtime.block_on(async {
            let report = query(pid, false).await.expect("blocked dispatcher dump");
            assert_eq!(report.current.expect("current").stage, Some("AppEvent"));
            let reset = query(pid, true).await.expect("reset without UI progress");
            assert!(reset.reset_pending);
            assert_eq!(reset.current.expect("still current").entry_epoch, 1);
            assert!(reset.stages.is_empty());
        });
        resume.send(()).expect("resume UI");
        observer
            .recv_timeout(Duration::from_secs(5))
            .expect("RPC stage entered");
        runtime.block_on(async {
            let report = query(pid, false).await.expect("pending RPC dump");
            assert_eq!(report.current.expect("current").stage, Some("RpcWait"));
            assert!(!report.reset_pending);
            assert!(report.stages.iter().all(|stage| stage.stage != "AppEvent"));
        });
        rpc_done.send(()).expect("finish RPC");
    });
    WRITER
        .scope(RefCell::new(Writer::new(Arc::clone(&shared))), async {
            {
                let _handler = enter(Stage::AppEvent);
                entered.send(()).expect("notify observer");
                resumed
                    .recv_timeout(Duration::from_secs(5))
                    .expect("independent control answered");
            }
            let unrelated = tokio::spawn(async {
                let _not_ui = enter(Stage::FramePaint);
            });
            {
                let _rpc = enter(Stage::RpcWait);
                entered.send(()).expect("notify RPC observer");
                rpc_pending.await.expect("RPC release");
            }
            unrelated.await.expect("background task");
        })
        .await;
    reader.join().expect("observer");
    let report = query(pid, false).await.expect("final snapshot");
    assert!(
        report
            .stages
            .iter()
            .all(|stage| stage.stage != "FramePaint")
    );
    assert_eq!(
        report
            .stages
            .iter()
            .find(|stage| stage.stage == "RpcWait")
            .expect("RPC completion")
            .count,
        1
    );

    // Slow and malformed clients cannot leave the one listener occupied forever.
    let path = codex_uds::prepare_user_runtime_directory()
        .expect("private runtime")
        .join(format!(
            "ui-latency-{pid}-{}.sock",
            report.identity.start_ticks
        ));
    let mut slow = std::os::unix::net::UnixStream::connect(&path).expect("slow client");
    use std::io::Write;
    slow.write_all(b"U").expect("partial command");
    query(pid, false)
        .await
        .expect("deadline releases slow client");
    let mut malformed = std::os::unix::net::UnixStream::connect(&path).expect("malformed client");
    malformed.write_all(b"NOTVALID").expect("bad command");
    query(pid, false)
        .await
        .expect("malformed client does not kill listener");
    drop(endpoint);
    assert!(!path.exists(), "normal exit unlinks only owned endpoint");
}

// An interrupted publisher and reset must never require writer progress.
// Repeated snapshots race real publication; accepted words must be one version.
#[test]
fn publication_reset_race_is_bounded_and_never_accepts_torn_records() {
    let shared = Arc::new(Shared::new());
    let record = Arc::new(Record::<8>::new());
    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let publisher = {
        let record = Arc::clone(&record);
        let finished = Arc::clone(&finished);
        std::thread::spawn(move || {
            for value in 1..10_000 {
                record.store([value; 8]);
            }
            finished.store(true, Ordering::Release);
        })
    };
    while !finished.load(Ordering::Acquire) {
        if let Some(words) = record.read() {
            assert!(words.iter().all(|word| *word == words[0]));
        }
    }
    publisher.join().expect("publisher");
    record.version.store(3, Ordering::Relaxed);
    assert!(
        record.read().is_none(),
        "interrupted publication returns immediately"
    );
    let mut writer = Writer::new(Arc::clone(&shared));
    let old = writer.enter(Stage::AppEvent).expect("stage admitted");
    shared.requested_epoch.store(2, Ordering::Relaxed);
    writer.leave(old);
    assert_eq!(shared.active_epoch.load(Ordering::Acquire), 2);
    assert_eq!(
        writer.stats[Stage::AppEvent as usize][1],
        0,
        "old in-flight span excluded"
    );

    let depths: Vec<_> = (0..DEPTH)
        .map(|_| writer.enter(Stage::FramePaint).expect("stage admitted"))
        .collect();
    assert!(writer.enter(Stage::FramePaint).is_none());
    assert_eq!(shared.suppressed_scopes.load(Ordering::Relaxed), 1);
    for depth in depths.into_iter().rev() {
        writer.leave(depth);
    }

    // Dropping a scoped dispatcher future unwinds its live guard in task-local
    // context; it must not poison the next scope or leak writer authority.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        WRITER
            .scope(RefCell::new(Writer::new(Arc::clone(&shared))), async {
                let input = crate::tui::TuiEvent::Paste(String::new());
                let old = input_consumed(&input).expect("consumed input");
                shared.requested_epoch.fetch_add(1, Ordering::Relaxed);
                input_handled(old);
                drop(enter(Stage::FramePaint));
                drop(enter(Stage::TerminalFlush));
                assert_eq!(
                    WRITER.with(|writer| writer.borrow().stats[Stage::ReactionLatency as usize][1]),
                    0,
                    "reset discards an input still waiting for its frame"
                );

                let mut first = input_consumed(&input).expect("first input");
                // Synthetic starts expose replacing the oldest input without
                // sleeping or relying on scheduler timing.
                first.0.start = 0;
                let first_start = first.0.start;
                let second = input_consumed(&input).expect("second input");
                assert!(second.0.start > first_start);
                drop(enter(Stage::FramePaint));
                drop(enter(Stage::TerminalFlush));
                assert_eq!(
                    WRITER.with(|writer| writer.borrow().stats[Stage::ReactionLatency as usize][1]),
                    0,
                    "pre-key paint cannot acknowledge an unfinished input handler"
                );
                // Handling order need not equal consumption order: the slot
                // still keeps the oldest consumed input.
                input_handled(second);
                input_handled(first);
                tokio::spawn(async {
                    assert!(input_consumed(&crate::tui::TuiEvent::Paste(String::new())).is_none());
                    drop(enter(Stage::TerminalFlush));
                })
                .await
                .expect("non-UI task");
                assert!(WRITER.with(|writer| writer.borrow().ready_input.is_some()));
                drop(enter(Stage::FramePaint));
                drop(enter(Stage::TerminalFlush));
                let committed = shared.committed.read().expect("reaction publication");
                assert_eq!(committed[2], Stage::ReactionLatency as u64 + 1);
                assert_eq!(committed[3], committed[1] - first_start);
                drop(enter(Stage::TerminalFlush));
                WRITER.with(|writer| {
                    let writer = writer.borrow();
                    let stats = writer.stats[Stage::ReactionLatency as usize];
                    assert_eq!(
                        stats[1], 1,
                        "one reaction per frame, not per input or flush"
                    );
                    assert_eq!(stats[4..].iter().sum::<u64>(), 1);
                    assert!(writer.ready_input.is_none());
                });
            })
            .await;
        let future = WRITER.scope(RefCell::new(Writer::new(Arc::clone(&shared))), async {
            let _handler = enter(Stage::AppEvent);
            std::future::pending::<()>().await;
        });
        tokio::pin!(future);
        assert!(futures::poll!(&mut future).is_pending());
    });
    assert!(enter(Stage::FramePaint).0.is_none());
}

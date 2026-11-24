use std::time::Duration;

use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_protocol::ThreadId;

use crate::app::tests::make_test_app_with_channels;
use crate::app::tests::scripted_server::scripted_server;
use crate::app::tests::test_thread_session;
use crate::app_event::AppEvent;
use crate::app_server_session::ThreadParamsMode;
use crate::pager_overlay::Overlay;

#[tokio::test]
async fn linked_thread_read_uses_the_captured_connection_without_replacing_the_draft()
-> color_eyre::Result<()> {
    let (mut app, mut events, _ops) = Box::pin(make_test_app_with_channels()).await;
    let source = ThreadId::new();
    let target = ThreadId::new();
    app.chat_widget
        .handle_thread_session(test_thread_session(source, app.config.cwd.to_path_buf()));
    app.chat_widget
        .apply_external_edit("Keep this draft".into());
    let mut tui = crate::tui::test_support::make_test_tui()?;
    tui.set_owned_screen(/*owned*/ false)?;
    app.open_transcript_overlay(&mut tui);
    let (mut server, mut commands, _service) = scripted_server(ThreadParamsMode::Remote).await?;

    app.request_thread_link(&server, Some(source), target);
    let request = loop {
        if let AppEvent::OpenThreadLink(request) =
            tokio::time::timeout(Duration::from_secs(/*secs*/ 5), events.recv())
                .await?
                .expect("thread link request")
        {
            break request;
        }
    };
    for _ in 0..2 {
        Box::pin(app.handle_event(&mut tui, &mut server, AppEvent::OpenThreadLink(request)))
            .await?;
    }
    let (result, ()) = tokio::join!(app.settle_turn_submissions(&mut server), async {
        let command = tokio::time::timeout(Duration::from_secs(/*secs*/ 5), commands.recv())
            .await
            .expect("linked-thread metadata read timed out")
            .expect("read on the issuing backend");
        let ClientRequest::ThreadRead { params, .. } = *command.request else {
            panic!("opening a link must first read metadata on the issuing backend");
        };
        assert_eq!(
            (params.thread_id, params.include_turns),
            (target.to_string(), false)
        );
        command
            .response
            .send(Ok(Err(JSONRPCErrorError {
                code: -32600,
                message: "not found".into(),
                data: None,
            })))
            .expect("read cancellation before reply");
    },);
    result?;
    // An already queued click must not be retried on a different connection era.
    app.reconnect.connection_epoch.advance();
    Box::pin(app.handle_event(&mut tui, &mut server, AppEvent::OpenThreadLink(request))).await?;
    app.settle_turn_submissions(&mut server).await?;
    assert!(
        matches!(
            commands.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "duplicate and stale clicks cannot issue another lookup"
    );
    assert!(matches!(app.overlay, Some(Overlay::Transcript(_))));
    assert_eq!(
        (
            app.current_displayed_thread_id(),
            app.chat_widget.composer_text_with_pending(),
        ),
        (Some(source), "Keep this draft".into()),
    );
    server.shutdown().await?;
    Ok(())
}

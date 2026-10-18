//! Invocation panes use the transcript's shared preparation pool and presentation barrier.

use crate::transcript_view::TranscriptView;
use crate::tui::FrameRequester;

impl TranscriptView {
    pub(crate) fn enable_background_preparation(&mut self, frames: FrameRequester) {
        frames.enable_background_preparation();
        self.cache.frames = Some(frames);
    }

    /// Reset conversation-local state without turning a live UI back into a synchronous view.
    pub(crate) fn reset(&mut self) {
        let frames = self.cache.frames.clone();
        *self = Self::default();
        self.cache.frames = frames;
    }
}

#[cfg(test)]
mod tests {
    use crate::history_cell::HistoryCell;
    use crate::history_cell::invocation::Action;
    use crate::history_cell::invocation::Panel;
    use crate::history_cell::invocation::Port;
    use crate::history_cell::output_provenance::OutputMode;
    use crate::transcript_view::TranscriptView;
    use crate::transcript_view::tests::render;
    use crate::transcript_view::tests::text;
    use crate::tui::FrameRequester;
    use codex_protocol::ToolName;
    use codex_protocol::tool_input::ToolInput;
    use codex_protocol::tool_input::ToolInputPayload;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn script_panes_settle_off_dispatcher_and_reject_stale_completions() {
        #[derive(Debug)]
        struct LiveCell(&'static str);
        impl HistoryCell for LiveCell {
            fn display_lines(&self, _width: u16) -> Vec<ratatui::text::Line<'static>> {
                vec![self.0.into()]
            }
            fn raw_lines(&self) -> Vec<ratatui::text::Line<'static>> {
                vec![self.0.into()]
            }
            fn activity_ids(&self) -> Vec<String> {
                vec![self.0.to_owned()]
            }
            fn is_tool_entry(&self) -> bool {
                false
            }
        }
        let frames = FrameRequester::test_dummy();
        let mut releases = Vec::new();
        let (started, entered) = std::sync::mpsc::channel();
        let mut blockers = Vec::new();
        for _ in 0..2 {
            let (release, blocked) = std::sync::mpsc::channel::<()>();
            releases.push(release);
            let started = started.clone();
            let mut task =
                crate::transcript_view::preparation::Pending::new(frames.clone(), move || {
                    started.send(()).unwrap();
                    let _ = blocked.recv();
                });
            task.poll();
            blockers.push(task);
        }
        for _ in 0..2 {
            entered.recv_timeout(Duration::from_secs(30)).unwrap();
        }
        let source = "print('unchanged source')\n".repeat(100);
        let input = ToolInput {
            id: "input:python".into(),
            call_id: "python".into(),
            name: ToolName::plain("python"),
            payload: ToolInputPayload::Text(source.clone()),
        };
        let cell = Arc::new(
            crate::history_cell::ToolOutputCell::from_output(
                "python-result",
                Some("python"),
                "python",
                None,
                &codex_protocol::models::FunctionCallOutputBody::Text("result".into()),
            )
            .unwrap(),
        ) as Arc<dyn HistoryCell>;
        let cells = [cell];
        let mut view = TranscriptView::default();
        view.record_tool_input(input);
        let original = render(&mut view, &cells, 80, 12);
        view.enable_background_preparation(frames.clone());
        // Commit the already-cached initial presentation before blocking its replacement.
        assert_eq!(text(&render(&mut view, &cells, 80, 12)), text(&original));
        // Opening a pane cannot replace truthful content with an estimated reservation.
        view.invocations.apply(
            "python",
            Action::Toggle(Port::Input, Panel::Visible(OutputMode::Raw)),
        );
        view.cache.clear();
        assert_eq!(text(&render(&mut view, &cells, 80, 12)), text(&original));
        assert!(text(&render(&mut view, &cells, 40, 12)).trim().is_empty());
        let mut live = TranscriptView::default();
        live.enable_background_preparation(frames);
        let key = |revision| {
            Some(crate::chatwidget::ActiveCellTranscriptKey {
                cacheable: true,
                revision,
                is_stream_continuation: false,
                is_tool_entry: false,
                animation_tick: None,
            })
        };
        let snapshot = |text: &'static str| crate::transcript_view::OwnedLiveTail {
            active: Some(Arc::new(LiveCell(text))),
            auxiliary: Vec::new(),
        };
        live.sync_owned_live_tail(80, key(1), false, || snapshot("first revision"));
        live.sync_owned_live_tail(80, key(2), false, || {
            panic!("an in-flight revision must finish before it is replaced")
        });
        assert!(text(&render(&mut live, &[], 80, 12)).trim().is_empty());
        drop(releases);
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            render(&mut view, &cells, 40, 12);
            if let Some(layout) = view.current_layout(&cells, 0) {
                assert_eq!(layout.text(), format!("Script (python)\n{source}"));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "layout did not settle"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!text(&render(&mut view, &cells, 40, 12)).contains("Preparing input"));
        let mut saw_first = false;
        live.sync_live_activity(&[], vec!["second revision".into()]);
        loop {
            live.sync_owned_live_tail(80, key(2), false, || snapshot("second revision"));
            let frame = text(&render(&mut live, &[], 80, 12));
            if frame.contains("first revision") {
                saw_first = true;
                assert_eq!(live.visible[0].activity_ids.as_ref(), &["first revision"]);
            }
            if frame.contains("second revision") {
                assert!(
                    saw_first,
                    "continuous revisions must not starve an already prepared presentation"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "live revisions did not settle"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        live.reset();
        assert!(live.cache.frames.is_some());
        assert!(live.live.is_none());
        assert!(live.presented.is_none());
    }
}

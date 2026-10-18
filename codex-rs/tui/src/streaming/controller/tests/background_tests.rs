//! Worker trials extend the unchanged inline streaming contract.

use super::*;
use pretty_assertions::assert_eq;

pub(super) struct Trial {
    background: StreamController,
    ready_revisions: tokio::sync::mpsc::UnboundedReceiver<(
        Vec<HyperlinkLine>,
        Option<usize>,
        HistoryRenderMode,
    )>,
    prepared: Vec<Line<'static>>,
}

impl Trial {
    pub(super) fn new(width: Option<usize>) -> Self {
        let (revisions, ready_revisions) = tokio::sync::mpsc::unbounded_channel();
        let background = stream_controller(width)
            .with_background(crate::tui::FrameRequester::test_dummy())
            .with_rewraps(std::sync::Arc::new(move |rows, width, mode| {
                revisions.send((rows, width, mode)).unwrap();
            }));
        Self {
            background,
            ready_revisions,
            prepared: Vec::new(),
        }
    }

    pub(super) async fn observe(&mut self, delta: &str, emitted_lines: &[Line<'static>]) {
        let Self {
            background,
            prepared,
            ..
        } = self;
        background.push(delta);
        background.core.settle().await;
        let (cell, idle) = background.on_commit_tick_batch(usize::MAX);
        assert!(idle);
        if let Some(cell) = cell {
            prepared.extend(cell.transcript_lines(u16::MAX).into_iter().map(|line| {
                let plain: String = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                Line::from(plain.chars().skip(2).collect::<String>())
            }));
        }
        assert_eq!(
            prepared.as_slice(),
            emitted_lines,
            "worker must publish every baseline stable row after {delta:?}"
        );
    }

    pub(super) async fn finish(self, source: &str) {
        let Self {
            mut background,
            mut ready_revisions,
            ..
        } = self;
        background.set_width(Some(48));
        background.core.settle().await;
        let (rows, width, mode) = ready_revisions.recv().await.unwrap();
        assert_eq!(width, Some(48));
        assert_eq!(mode, HistoryRenderMode::Rich);
        let expected = crate::markdown::render_markdown_agent_with_list_spacing(
            source,
            width,
            Some(&test_cwd()),
            None,
            ListSpacing::AfterMultiline,
        );
        assert_eq!(
            hyperlink_lines_to_plain_strings(&rows),
            hyperlink_lines_to_plain_strings(&expected)
        );
        assert!(background.on_commit_tick_batch(usize::MAX).0.is_none());
        let emphasis = "\n**open emphasis\n";
        background.push(emphasis);
        background.core.settle().await;
        let (held, _) = background.on_commit_tick_batch(usize::MAX);
        assert!(held.is_none(), "open emphasis cannot become immutable rows");
        let closed = "closed**\n\n";
        background.push(closed);
        background.core.settle().await;
        let (released, _) = background.on_commit_tick_batch(usize::MAX);
        let released = released.expect("closed emphasis streams without waiting for finalization");
        assert!(
            released.is_stream_continuation(),
            "rewrap already emitted the first-row header"
        );
        let reference = "[unresolved][id]\n\nfollowing block\n";
        background.push(reference);
        background.core.settle().await;
        let (held, _) = background.on_commit_tick_batch(usize::MAX);
        assert!(
            held.is_none(),
            "an unresolved reference holds subsequent blocks too"
        );
        let definition = "\n[id]: https://example.com\n\n";
        background.push(definition);
        background.core.settle().await;
        assert!(background.on_commit_tick_batch(usize::MAX).0.is_some());
        let ui = std::thread::current().id();
        let (tx, rx) = tokio::sync::oneshot::channel();
        assert!(
            background
                .finish_background(move |source| {
                    let _ = tx.send((source, std::thread::current().id()));
                })
                .is_ok()
        );
        let (final_source, worker) = rx.await.unwrap();
        assert_eq!(
            final_source,
            format!("{source}{emphasis}{closed}{reference}{definition}")
        );
        assert_ne!(worker, ui);
        for (paragraph, mode) in [
            "A long unterminated paragraph streams complete words line by line without rolling or repainting earlier rows, even with **closed strong words** and a [stable link](https://example.com) followed by more ordinary prose that wraps repeatedly.",
            "- A long list item streams complete words line by line without rolling or repainting earlier rows, even with **closed strong words** followed by more ordinary prose that wraps repeatedly.",
        ].into_iter().flat_map(|paragraph| {
            [HistoryRenderMode::Rich, HistoryRenderMode::Raw].map(|mode| (paragraph, mode))
        }) {
            let mut words = stream_controller(Some(32));
            words.set_render_mode(mode);
            let mut words = words.with_background(crate::tui::FrameRequester::test_dummy());
            let render = |source: &str| match mode {
                HistoryRenderMode::Rich => crate::markdown::render_markdown_agent_with_list_spacing(
                    source, Some(32), Some(&test_cwd()), None, ListSpacing::AfterMultiline,
                ),
                HistoryRenderMode::Raw => crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
                    &history_cell::raw_lines_from_source(source).into_iter().map(HyperlinkLine::new).collect::<Vec<_>>(),
                    crate::wrapping::RtOptions::new(32),
                ),
            };
            let canonical = render(paragraph);
            let mut immutable = Vec::new();
            let mut arrived = String::new();
            for word in paragraph.split_inclusive(' ') {
                arrived.push_str(word);
                words.push(word);
                words.core.settle().await;
                immutable.extend(words.core.tick_batch(usize::MAX));
                let tail = words.current_tail_lines();
                assert!(tail.len() <= 1, "only the growing last row remains live");
                for (actual, expected) in immutable.iter().zip(&canonical) {
                    assert_eq!(
                        actual.line, expected.line,
                        "stable prefix after {arrived:?}"
                    );
                    assert_eq!(actual.hyperlinks, expected.hyperlinks);
                }
                assert!(immutable.len() < canonical.len());
                if let Some(tail) = tail.first() {
                    let actual: String = tail
                        .line
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect();
                    let expected: String = canonical[immutable.len()]
                        .line
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect();
                    assert!(
                        expected.starts_with(actual.trim_end()),
                        "tail {actual:?} is not a prefix of {expected:?}"
                    );
                }
            }
            assert!(
                immutable.len() >= 3,
                "unterminated long lines stream before finalization"
            );
            words.push("\n");
            words.core.settle().await;
            immutable.extend(words.core.tick_batch(usize::MAX));
            assert_eq!(immutable.len(), canonical.len());
            for (published, final_row) in immutable.iter().zip(&canonical) {
                assert_eq!(published.line, final_row.line);
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            assert!(
                words
                    .finish_background(move |source| {
                        let _ = tx.send(source);
                    })
                    .is_ok()
            );
            let finalized = rx.await.unwrap();
            assert_eq!(finalized, format!("{paragraph}\n"));
            let final_rows = render(&finalized);
            for (published, finalized) in immutable.iter().zip(final_rows) {
                assert_eq!(
                    published.line, finalized.line,
                    "finalization must preserve published rows"
                );
                assert_eq!(published.hyperlinks, finalized.hyperlinks);
            }
        }
        let chunk_source = "Immutable worker chunks preserve prefix sums and row order without moving individual rows on the UI thread. ".repeat(3);
        let mut chunks =
            stream_controller(Some(32)).with_background(crate::tui::FrameRequester::test_dummy());
        for word in chunk_source.split_inclusive(' ') {
            chunks.push(word);
            chunks.core.settle().await;
        }
        let (cell, _) = chunks.on_commit_tick_batch(usize::MAX);
        let cell = cell.expect("one emission transfers every completed chunk");
        let shared = cell
            .as_any()
            .downcast_ref::<history_cell::StreamRowsCell>()
            .unwrap();
        let history_cell::StreamRowContent::Shared { rows, .. } = &shared.content else {
            panic!("background emission retains shared worker chunks");
        };
        let mut expected = crate::markdown::render_markdown_agent_with_list_spacing(
            &chunk_source,
            Some(32),
            Some(&test_cwd()),
            None,
            ListSpacing::AfterMultiline,
        );
        expected.pop();
        assert!(rows.len() > 3);
        assert_eq!(
            hyperlink_lines_to_plain_strings(&rows.materialize()),
            hyperlink_lines_to_plain_strings(&expected)
        );
        for suffix in [
            "| Value\n|---|---|\n| item | data |\n\nFollowing block.\n",
            "\n---\n\n",
        ] {
            let prefix = "A long initially plain paragraph publishes stable rows before its structure changes ";
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let mut revised = stream_controller(Some(32))
                .with_background(crate::tui::FrameRequester::test_dummy())
                .with_rewraps(std::sync::Arc::new(move |rows, _, _| {
                    tx.send(rows).unwrap();
                }));
            revised.push(prefix);
            revised.core.settle().await;
            assert!(!revised.core.tick_batch(usize::MAX).is_empty());
            revised.set_width(Some(24));
            revised.core.settle().await;
            let mut resized = rx.recv().await.unwrap();
            resized.extend(revised.current_tail_lines());
            let canonical = crate::markdown::render_markdown_agent_with_list_spacing(
                prefix,
                Some(24),
                Some(&test_cwd()),
                None,
                ListSpacing::AfterMultiline,
            );
            assert_eq!(
                hyperlink_lines_to_plain_strings(&resized),
                hyperlink_lines_to_plain_strings(&canonical)
            );
            if let Some(suffix) = suffix.strip_prefix('|') {
                let held = revised.current_tail_lines();
                revised.push("|");
                revised.core.settle().await;
                assert!(revised.core.tick_batch(usize::MAX).is_empty());
                assert_eq!(
                    revised.current_tail_lines(),
                    held,
                    "late pipe freezes the line"
                );
                revised.push(suffix);
            } else {
                revised.push(suffix);
            }
            revised.core.settle().await;
            let replacement = rx.recv().await.unwrap();
            let canonical = crate::markdown::render_markdown_agent_with_list_spacing(
                &format!("{prefix}{suffix}"),
                Some(24),
                Some(&test_cwd()),
                None,
                ListSpacing::AfterMultiline,
            );
            assert_eq!(
                hyperlink_lines_to_plain_strings(&replacement),
                hyperlink_lines_to_plain_strings(&canonical)
            );
            assert!(revised.core.tick_batch(usize::MAX).is_empty());
        }
    }
}

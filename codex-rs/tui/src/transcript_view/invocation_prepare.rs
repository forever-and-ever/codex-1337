//! Cold script panes are settled off the dispatcher, including wrapping and copy geometry.
//! One process-lived worker owns a two-request channel; abandoned requests never publish.
//! Headless views stay synchronous until their UI supplies a frame requester.

use super::Choices;
use super::Invocations;
use super::PreparationSource;
use super::SourceRevision;
use crate::history_cell::HistoryCell;
use crate::history_cell::invocation::Invocation;
use crate::history_cell::invocation::Panel;
use crate::history_cell::invocation::collect;
use crate::history_cell::output_provenance::client_notice;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::transcript_view::TranscriptView;
use crate::transcript_view::layout::{CellPresentation, RenderState};
use crate::transcript_view::text::HeaderRows;
use crate::transcript_view::text::TextLayout;
use crate::transcript_view::tool_output_tokens::ToolOutputTokens;
use crate::tui::FrameRequester;
use codex_protocol::tool_input::ToolInput;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::Weak;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

pub(in crate::transcript_view) enum PreparedLayout {
    Ready(TextLayout),
    Cached(Arc<TextLayout>),
    Pending(Arc<TextLayout>),
}

impl From<TextLayout> for PreparedLayout {
    fn from(layout: TextLayout) -> Self {
        Self::Ready(layout)
    }
}

struct Call {
    id: String,
    input: Option<Arc<ToolInput>>,
    choices: Choices,
}

struct Key {
    source: SourceRevision,
    width: u16,
    presentation: CellPresentation,
    render: RenderState,
    calls: Vec<Call>,
    tokens: ToolOutputTokens,
    expanded: HashSet<String>,
}

impl Key {
    fn matches(&self, other: &Self) -> bool {
        (match (&self.source, &other.source) {
            (SourceRevision::Retained(a), SourceRevision::Retained(b)) => a.ptr_eq(b),
            (SourceRevision::Live(a), SourceRevision::Live(b)) => a == b,
            _ => false,
        }) && self.width == other.width
            && self.presentation == other.presentation
            && self.render == other.render
            && self.tokens == other.tokens
            && self.expanded == other.expanded
            && self.calls.len() == other.calls.len()
            && self.calls.iter().zip(&other.calls).all(|(a, b)| {
                a.id == b.id
                    && a.choices == b.choices
                    && match (&a.input, &b.input) {
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        (None, None) => true,
                        _ => false,
                    }
            })
    }
}

struct Job {
    owner: Weak<()>,
    render: Box<dyn FnOnce() -> (TextLayout, Option<Arc<TextLayout>>) + Send>,
    result: mpsc::Sender<(TextLayout, Option<Arc<TextLayout>>)>,
    frames: FrameRequester,
}

struct Worker {
    requests: SyncSender<Job>,
    // The worker and its join handle have process lifetime, like the syntax catalogue.
    // Requests own no files or child processes; process exit closes the channel/thread.
    _thread: JoinHandle<()>,
}

impl Worker {
    fn get() -> Result<&'static Self, &'static str> {
        static WORKER: OnceLock<Result<Worker, String>> = OnceLock::new();
        WORKER
            .get_or_init(|| {
                let (requests, incoming) = mpsc::sync_channel::<Job>(2);
                let thread = std::thread::Builder::new()
                    .name("script-preparation".into())
                    .spawn(move || {
                        while let Ok(job) = incoming.recv() {
                            if job.owner.strong_count() != 0 {
                                let layout = (job.render)();
                                let _ = job.result.send(layout);
                            }
                            job.frames.schedule_frame();
                        }
                    })
                    .map_err(|error| error.to_string())?;
                Ok(Worker {
                    requests,
                    _thread: thread,
                })
            })
            .as_ref()
            .map_err(String::as_str)
    }
}

struct Pending {
    key: Key,
    placeholder: Arc<TextLayout>,
    // The unsubmitted job is retained only while the bounded worker queue is full.
    queued: Option<Job>,
    result: Receiver<(TextLayout, Option<Arc<TextLayout>>)>,
    _owner: Arc<()>,
}

#[derive(Default)]
pub(super) struct Background {
    frames: Option<FrameRequester>,
    pending: Vec<Pending>,
    live: Option<(Key, Arc<TextLayout>, Arc<TextLayout>)>,
}

impl Background {
    pub(super) fn invalidate(&mut self, id: &str) {
        self.pending
            .retain(|entry| !entry.key.calls.iter().any(|call| call.id == id));
        if self
            .live
            .as_ref()
            .is_some_and(|(key, _, _)| key.calls.iter().any(|call| call.id == id))
        {
            self.live = None;
        }
    }

    pub(super) fn live_separated(&self, layout: &Arc<TextLayout>) -> Option<Arc<TextLayout>> {
        self.live
            .as_ref()
            .filter(|(_, plain, _)| Arc::ptr_eq(plain, layout))
            .map(|(_, _, separated)| Arc::clone(separated))
    }
}

impl TranscriptView {
    pub(crate) fn enable_background_preparation(&mut self, frames: FrameRequester) {
        self.invocations.background.borrow_mut().frames = Some(frames);
    }
}

impl Invocations {
    pub(in crate::transcript_view) fn defer(
        &self,
        cell: &dyn HistoryCell,
        source: PreparationSource<'_>,
        snapshot: impl FnOnce() -> Option<Arc<dyn HistoryCell>>,
        auxiliary: Vec<HyperlinkLine>,
        width: u16,
        presentation: CellPresentation,
        open: bool,
        placement: crate::transcript_view::feed::Content,
        full: bool,
        tokens: &ToolOutputTokens,
        expanded: &HashSet<String>,
        header: impl FnOnce() -> Option<HeaderRows>,
    ) -> Option<PreparedLayout> {
        let mut background = self.background.borrow_mut();
        let frames = background.frames.as_ref()?.clone();
        if full || matches!(source, PreparationSource::Uncached) {
            return None;
        }
        let calls = collect(cell);
        let costly = calls.iter().any(|call| {
            matches!(
                self.choices.get(call.id).and_then(|choice| choice.input),
                Some(Panel::Visible(_))
            ) && call.script_language(self.input(call.id)).is_some()
                && call.raw_source(self.input(call.id)).is_some()
        });
        if !costly {
            return None;
        }
        let output_ids: Vec<_> = calls.iter().flat_map(Invocation::output_ids).collect();
        let key = Key {
            source: match source {
                PreparationSource::Retained(cell) => SourceRevision::Retained(Arc::downgrade(cell)),
                PreparationSource::Live(revision) => SourceRevision::Live(revision),
                PreparationSource::Uncached => unreachable!(),
            },
            width,
            presentation,
            render: RenderState::current(),
            calls: calls
                .iter()
                .map(|call| Call {
                    id: call.id.to_owned(),
                    input: self.inputs.get(call.id).cloned(),
                    choices: self.choices.get(call.id).copied().unwrap_or_default(),
                })
                .collect(),
            tokens: tokens.subset(&output_ids),
            expanded: expanded.clone(),
        };
        if let Some((cached, layout, _)) = &background.live
            && cached.matches(&key)
        {
            return Some(PreparedLayout::Cached(Arc::clone(layout)));
        }
        let index = match background
            .pending
            .iter()
            .position(|entry| entry.key.matches(&key))
        {
            Some(index) => index,
            None => {
                // A changed width/source/choice must not consume an earlier completion.
                background
                    .pending
                    .retain(|entry| match (&entry.key.source, &key.source) {
                        (SourceRevision::Retained(a), SourceRevision::Retained(b)) => {
                            a.strong_count() != 0 && !a.ptr_eq(b)
                        }
                        (SourceRevision::Live(_), SourceRevision::Live(_)) => false,
                        _ => true,
                    });
                if background.pending.len() == 32 {
                    background.pending.remove(0);
                }
                let snapshot = snapshot()?;
                let header = header();
                let mut placeholder = self
                    .layout_inner(
                        cell,
                        source,
                        width,
                        open,
                        placement,
                        full,
                        tokens,
                        expanded,
                        Vec::new(),
                        true,
                    )
                    .expect("script pane has an invocation");
                if let Some(header) = &header {
                    placeholder = placeholder.with_fold_header(header.clone());
                }
                let placeholder = Arc::new(presentation.decorate(placeholder));
                let inputs = key
                    .calls
                    .iter()
                    .filter_map(|call| {
                        call.input
                            .as_ref()
                            .map(|input| (call.id.clone(), Arc::clone(input)))
                    })
                    .collect();
                let choices = key
                    .calls
                    .iter()
                    .map(|call| (call.id.clone(), call.choices))
                    .collect();
                let tokens = key.tokens.clone();
                let expanded = key.expanded.clone();
                let source = snapshot;
                let live = matches!(key.source, SourceRevision::Live(_));
                let (result, receiver) = mpsc::channel();
                let owner = Arc::new(());
                let job = Job {
                    owner: Arc::downgrade(&owner),
                    result,
                    frames: frames.clone(),
                    render: Box::new(move || {
                        let invocations = Invocations {
                            inputs,
                            choices,
                            ..Default::default()
                        };
                        let mut layout = invocations
                            .layout(
                                source.as_ref(),
                                PreparationSource::Retained(&source),
                                width,
                                open,
                                placement,
                                full,
                                &tokens,
                                &expanded,
                                auxiliary,
                            )
                            .expect("script pane has an invocation");
                        if let Some(header) = header {
                            layout = layout.with_fold_header(header);
                        }
                        let separated =
                            live.then(|| Arc::new(layout.rewrap(width).with_leading_separator()));
                        (layout, separated)
                    }),
                };
                background.pending.push(Pending {
                    key,
                    placeholder,
                    queued: Some(job),
                    result: receiver,
                    _owner: owner,
                });
                background.pending.len() - 1
            }
        };
        let entry = &mut background.pending[index];
        if let Some(job) = entry.queued.take() {
            match Worker::get() {
                Ok(worker) => match worker.requests.try_send(job) {
                    Ok(()) => {}
                    Err(TrySendError::Full(job)) => {
                        entry.queued = Some(job);
                        frames.schedule_content_in(Duration::from_millis(50));
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        return Some(PreparedLayout::Ready(TextLayout::new(
                            vec![client_notice("Input preparation failed: worker stopped")],
                            width,
                        )));
                    }
                },
                Err(error) => {
                    return Some(PreparedLayout::Ready(TextLayout::new(
                        vec![client_notice(&format!("Input preparation failed: {error}"))],
                        width,
                    )));
                }
            }
        }
        match entry.result.try_recv() {
            Ok((layout, separated)) => {
                let entry = background.pending.remove(index);
                if matches!(entry.key.source, SourceRevision::Live(_)) {
                    let layout = Arc::new(layout);
                    background.live = Some((
                        entry.key,
                        Arc::clone(&layout),
                        separated.expect("live layout includes its separator variant"),
                    ));
                    Some(PreparedLayout::Cached(layout))
                } else {
                    Some(PreparedLayout::Ready(layout))
                }
            }
            Err(TryRecvError::Empty) => {
                Some(PreparedLayout::Pending(Arc::clone(&entry.placeholder)))
            }
            Err(TryRecvError::Disconnected) => Some(PreparedLayout::Ready(TextLayout::new(
                vec![client_notice("Input preparation failed: worker stopped")],
                width,
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_cell::ToolInputCell;
    use crate::history_cell::activity_parts::FoldTarget;
    use crate::history_cell::invocation::Action;
    use crate::history_cell::invocation::Port;
    use crate::history_cell::output_provenance::OutputMode;
    use crate::transcript_view::tests::{render, text};
    use codex_protocol::ToolName;
    use codex_protocol::tool_input::ToolInputPayload;

    #[test]
    fn script_panes_settle_off_dispatcher_and_reject_stale_completions() {
        let frames = FrameRequester::test_dummy();
        let worker = Worker::get().unwrap();
        let owner = Arc::new(());
        let (release, blocked) = mpsc::channel();
        let (started, entered) = mpsc::channel();
        let (discard, _) = mpsc::channel();
        worker
            .requests
            .send(Job {
                owner: Arc::downgrade(&owner),
                frames: frames.clone(),
                result: discard,
                render: Box::new(move || {
                    started.send(()).unwrap();
                    let _ = blocked.recv();
                    (TextLayout::new(Vec::new(), 80), None)
                }),
            })
            .ok()
            .unwrap();
        entered.recv_timeout(Duration::from_secs(30)).unwrap();
        // Holding the real worker proves that opening, resizing and closing a pane do
        // not execute its preparation inline. No scheduling-speed assertion is needed.
        let source = "print('unchanged source')\n".repeat(100);
        let input = ToolInput {
            id: "input:python".into(),
            call_id: "python".into(),
            name: ToolName::plain("python"),
            payload: ToolInputPayload::Text(source.clone()),
        };
        let live_cell = ToolInputCell(input.clone());
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
        let cells = [Arc::clone(&cell)];
        let mut view = TranscriptView::default();
        view.enable_background_preparation(frames.clone());
        view.record_tool_input(input.clone());
        view.invocations.apply(
            "python",
            Action::Toggle(Port::Input, Panel::Visible(OutputMode::Raw)),
        );
        assert!(text(&render(&mut view, &cells, 80, 12)).contains("Preparing input"));
        let abandoned = Arc::downgrade(&view.invocations.background.borrow().pending[0]._owner);
        assert!(text(&render(&mut view, &cells, 40, 12)).contains("Preparing input"));
        assert_eq!(abandoned.strong_count(), 0);
        let close = FoldTarget::Invocation {
            call_id: "python".into(),
            action: Action::Toggle(Port::Input, Panel::Hidden(OutputMode::Raw)),
        };
        assert!(view.focus_fold(&cells, 0, close));
        view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
        assert!(!text(&render(&mut view, &cells, 40, 12)).contains("Preparing input"));
        view.invocations.apply(
            "python",
            Action::Toggle(Port::Input, Panel::Visible(OutputMode::Raw)),
        );
        view.cache.clear();
        release.send(()).unwrap();

        let drain = || {
            let (result, done) = mpsc::channel();
            worker
                .requests
                .send(Job {
                    owner: Arc::downgrade(&owner),
                    frames: frames.clone(),
                    result,
                    render: Box::new(|| (TextLayout::new(Vec::new(), 1), None)),
                })
                .ok()
                .unwrap();
            done.recv_timeout(Duration::from_secs(30)).unwrap();
        };
        drain();
        render(&mut view, &cells, 40, 12);
        let settled = view.current_layout(&cells, 0).unwrap();
        assert_eq!(settled.text(), source);
        view.record_tool_input(ToolInput {
            id: "input:other".into(),
            call_id: "other".into(),
            ..input.clone()
        });
        render(&mut view, &cells, 40, 12);
        assert!(Arc::ptr_eq(
            &settled,
            &view.current_layout(&cells, 0).unwrap()
        ));

        // Animation and unrelated input do not rewrap a settled live script either.
        let mut live = TranscriptView::default();
        live.enable_background_preparation(frames.clone());
        live.record_tool_input(input);
        live.invocations.apply(
            "python",
            Action::Toggle(Port::Input, Panel::Visible(OutputMode::Raw)),
        );
        let key = |tick| {
            Some(crate::chatwidget::ActiveCellTranscriptKey {
                cacheable: true,
                revision: 7,
                is_stream_continuation: false,
                is_tool_entry: true,
                animation_tick: Some(tick),
            })
        };
        live.sync_live_invocation_tail(80, key(0), &live_cell, Vec::new());
        drain();
        live.sync_live_invocation_tail(80, key(0), &live_cell, Vec::new());
        let settled = Arc::clone(live.live.as_ref().unwrap());
        assert_eq!(settled.text(), source);
        live.sync_live_invocation_tail(80, key(1), &live_cell, Vec::new());
        assert!(Arc::ptr_eq(&settled, live.live.as_ref().unwrap()));
    }
}

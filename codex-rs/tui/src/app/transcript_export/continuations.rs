//! Background export preparation and UI progress receipts.

use super::App;
use super::AppServerSession;
use super::Path;
use super::PathBuf;
use super::RawReasoningVisibility;
use super::ThreadId;
use super::TranscriptExportDestination;
use super::TypedRequestError;
use super::Write;
use super::is_history_pagination_unsupported;
use super::load_export_transcript;
use super::render_markdown_transcript;

pub(super) fn prepare_transcript(
    cwd: &Path,
    requested_path: &Path,
    markdown: &str,
) -> Result<(tempfile::NamedTempFile, PathBuf), String> {
    let path = if let Ok(relative) = requested_path.strip_prefix("~") {
        dirs::home_dir()
            .ok_or_else(|| "could not determine the home directory".to_string())?
            .join(relative)
    } else if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        cwd.join(requested_path)
    };
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(cwd))
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    file.write_all(markdown.as_bytes())
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    Ok((file, path))
}

impl App {
    pub(super) fn export_transcript_continuation(
        &mut self,
        server: &AppServerSession,
        destination: TranscriptExportDestination,
    ) -> Result<(), String> {
        use super::super::turn_submissions::ui_requests::UiRequestKind;
        use super::super::turn_submissions::ui_requests::UiRequestScope;
        let thread_id = self
            .current_displayed_thread_id()
            .ok_or("No active conversation to export.")?;
        if self.active_export.is_some() {
            return Err("An export is already in progress; /export can cancel it.".into());
        }
        let id = uuid::Uuid::new_v4();
        let config = self.config.clone();
        let visibility = if config.show_raw_agent_reasoning {
            RawReasoningVisibility::Visible
        } else {
            RawReasoningVisibility::Hidden
        };
        let cells = self.transcript_cells.clone();
        let metadata = self
            .agent_navigation
            .tracked_thread_ids()
            .into_iter()
            .chain(self.thread_event_channels.keys().copied())
            .map(|id| (id, self.chat_widget.collab_agent_metadata(id)))
            .collect::<std::collections::HashMap<_, _>>();
        let cwd = if self.app_server_target.uses_remote_workspace() {
            self.launch_cwd.clone()
        } else {
            self.chat_widget.config_ref().cwd.to_path_buf()
        };
        let image_origin = if self.app_server_target.uses_remote_workspace() {
            crate::history_cell::ImagePathOrigin::Remote
        } else {
            crate::history_cell::ImagePathOrigin::Local
        };
        let tx = self.app_event_tx.clone();
        self.chat_widget.show_transcript_export_progress(0);
        let handle = self.request_server(
            server,
            UiRequestScope::Thread(thread_id),
            UiRequestKind::ReadOnly,
            "export conversation",
            move |handle| async move {
                let pages = std::sync::atomic::AtomicUsize::new(0);
                let progress = || {
                    let pages = pages.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    tx.send(crate::app_event::AppEvent::TranscriptExportProgress { id, pages });
                };
                let metadata = |id| metadata.get(&id).cloned().unwrap_or_default();
                let cells = load_export_transcript(
                    &handle,
                    thread_id,
                    visibility,
                    Some(&config),
                    cells,
                    &metadata,
                    &progress,
                    image_origin,
                )
                .await;
                Ok(cells
                    .and_then(|cells| render_markdown_transcript(&cells))
                    .and_then(|markdown| match destination {
                        TranscriptExportDestination::Clipboard => {
                            Ok(PreparedExport::Clipboard(markdown))
                        }
                        TranscriptExportDestination::File(path) => {
                            prepare_transcript(&cwd, &path, &markdown)
                                .map(|(file, path)| PreparedExport::File { file, path })
                        }
                    }))
            },
            move |app, _, result| {
                let result = result
                    .map_err(ToString::to_string)
                    .and_then(|result| result);
                app.app_event_tx
                    .send(crate::app_event::AppEvent::FinishTranscriptExport { id, result });
                Ok(())
            },
        );
        self.active_export = Some(ActiveExport {
            id,
            thread_id,
            epoch: self.server_operations.epoch(),
            pages: 0,
            handle,
        });
        Ok(())
    }

    pub(in crate::app) fn sync_frontend_requests(&mut self) {
        if self.active_export.as_ref().is_some_and(|active| {
            self.current_displayed_thread_id() != Some(active.thread_id)
                || self.server_operations.epoch() != active.epoch
        }) {
            self.cancel_transcript_export();
        }
        self.sync_prompt_revert();
    }

    pub(in crate::app) fn cancel_transcript_export(&mut self) {
        if let Some(active) = self.active_export.take() {
            active.handle.cancel();
            if self.chat_widget.thread_id() == Some(active.thread_id) {
                self.chat_widget.clear_transcript_export_progress();
                self.chat_widget
                    .add_info_message("Export canceled.".into(), None);
            }
        }
    }

    pub(in crate::app) fn transcript_export_progress(&mut self, id: uuid::Uuid, pages: usize) {
        let current = self.current_displayed_thread_id();
        let Some(active) = self.active_export.as_mut().filter(|active| active.id == id) else {
            return;
        };
        if current != Some(active.thread_id) || self.server_operations.epoch() != active.epoch {
            self.cancel_transcript_export();
            return;
        }
        active.pages = pages;
        self.chat_widget.show_transcript_export_progress(pages);
    }

    pub(in crate::app) fn finish_transcript_export(
        &mut self,
        tui: &mut crate::tui::Tui,
        id: uuid::Uuid,
        result: Result<PreparedExport, String>,
    ) {
        let Some(active) = self.active_export.as_ref().filter(|active| active.id == id) else {
            return;
        };
        if self.current_displayed_thread_id() != Some(active.thread_id)
            || self.server_operations.epoch() != active.epoch
        {
            self.cancel_transcript_export();
            return;
        }
        self.active_export = None;
        self.chat_widget.clear_transcript_export_progress();
        let result = result.and_then(|output| match output {
            PreparedExport::Clipboard(markdown) => {
                let result = tui.copy_transcript_selection(
                    &markdown,
                    crate::clipboard_copy::CopyFormat::PlainText,
                );
                self.chat_widget.show_copy_result("conversation", result);
                Ok(())
            }
            PreparedExport::File { file, path } => {
                file.persist_noclobber(&path)
                    .map_err(|error| format!("could not create {}: {error}", path.display()))?;
                self.chat_widget
                    .add_info_message(format!("Saved conversation to {}", path.display()), None);
                Ok(())
            }
        });
        if let Err(error) = result {
            self.chat_widget
                .add_error_message(format!("Export failed: {error}"));
        }
    }
}

pub(in crate::app) struct ActiveExport {
    id: uuid::Uuid,
    thread_id: ThreadId,
    epoch: u64,
    pub(in crate::app) pages: usize,
    handle: super::super::turn_submissions::ui_requests::UiRequestHandle,
}

#[derive(Debug)]
pub(crate) enum PreparedExport {
    Clipboard(String),
    File {
        file: tempfile::NamedTempFile,
        path: PathBuf,
    },
}

pub(super) async fn load_export_thread(
    handle: &codex_app_server_client::AppServerRequestHandle,
    thread_id: ThreadId,
    progress: &(dyn Fn() + Send + Sync),
    image_origin: crate::history_cell::ImagePathOrigin,
) -> Result<Option<codex_app_server_protocol::Thread>, String> {
    let prepared = crate::app_server_session::history_job::load(
        handle,
        thread_id,
        None,
        None,
        progress,
        image_origin,
    )
    .await;
    let thread = match prepared.result {
        Ok(thread) if thread.ephemeral => return Ok(None),
        Ok(thread) => thread,
        Err(error) => {
            if matches!(error.downcast_ref::<TypedRequestError>(), Some(TypedRequestError::Server { source, .. }) if is_history_pagination_unsupported(source))
            {
                match crate::app_server_session::history_job::legacy(handle, thread_id).await {
                    Ok(legacy) if !legacy.turns.is_empty() => legacy,
                    _ => return Ok(None),
                }
            } else {
                return Err(format!("could not load conversation history: {error}"));
            }
        }
    };
    Ok(Some(thread))
}

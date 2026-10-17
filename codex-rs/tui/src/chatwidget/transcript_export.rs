//! Destination and filename prompts for on-demand transcript exports.

use super::*;
use crate::app_event::TranscriptExportDestination;
use crate::bottom_pane::popup_consts::picker_hint_line_for_keymap;

impl ChatWidget {
    pub(super) fn show_transcript_export_popup(&mut self) {
        self.app_event_tx.send(AppEvent::OpenTranscriptExportMenu);
    }

    pub(crate) fn show_transcript_export_progress(&mut self, pages: usize) {
        self.bottom_pane.set_footer_hint_override(Some(vec![(
            String::new(),
            format!("Exporting… {pages} pages · /export to cancel"),
        )]));
        self.request_redraw();
    }

    pub(crate) fn clear_transcript_export_progress(&mut self) {
        self.bottom_pane.set_footer_hint_override(None);
        self.request_redraw();
    }

    pub(crate) fn show_transcript_export_cancel(&mut self, pages: usize) {
        self.show_selection_view(SelectionViewParams {
            title: Some(format!("Exporting conversation… {pages} pages")),
            items: vec![SelectionItem {
                name: "Cancel export".into(),
                actions: vec![Box::new(|tx| tx.send(AppEvent::CancelTranscriptExport))],
                dismiss_on_select: true,
                ..Default::default()
            }],
            ..SelectionViewParams::picker()
        });
    }

    pub(crate) fn show_transcript_export_options(&mut self) {
        self.show_selection_view(SelectionViewParams {
            header: Box::new(
                Paragraph::new(vec![
                    Line::from("Export conversation".bold()),
                    Line::from("Save the complete conversation as Markdown".dim()),
                ])
                .wrap(Wrap { trim: false }),
            ),
            footer_hint: Some(picker_hint_line_for_keymap(&self.bottom_pane.list_keymap())),
            items: vec![
                SelectionItem {
                    name: "Copy to clipboard".to_string(),
                    description: Some("Copy the complete Markdown transcript".to_string()),
                    is_disabled: cfg!(target_os = "android"),
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::ExportTranscript {
                            destination: TranscriptExportDestination::Clipboard,
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Save to file".to_string(),
                    description: Some("Choose a Markdown filename".to_string()),
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::OpenTranscriptExportFilePrompt);
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..SelectionViewParams::picker()
        });
        self.defer_input_until_settings_applied();
        self.request_redraw();
    }

    pub(crate) fn show_transcript_export_file_prompt(&mut self) {
        let tx = self.app_event_tx.clone();
        let filename = self.thread_id().map_or_else(
            || "codex-session.md".to_string(),
            |thread_id| format!("codex-session-{thread_id}.md"),
        );
        let view = CustomPromptView::new(
            "Save conversation".to_string(),
            "Type a filename and press Enter".to_string(),
            filename,
            /*context_label*/ None,
            Box::new(move |filename| {
                tx.send(AppEvent::ExportTranscript {
                    destination: TranscriptExportDestination::File(PathBuf::from(filename)),
                });
            }),
        );
        self.bottom_pane.show_text_prompt(view);
        self.request_redraw();
    }
}

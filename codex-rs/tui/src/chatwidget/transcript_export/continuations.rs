//! Export progress and cancellation controls.

use super::*;

impl ChatWidget {
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
}

use super::HistoryCell;
use crate::transcript_view::FeedEntry;
use crate::transcript_view::HeaderDisclosure;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;
use ratatui::style::Color;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::time::Duration;

#[derive(Debug)]
pub(crate) struct CompactionCell {
    pub(crate) id: String,
    pub(crate) elapsed: Option<Duration>,
}

impl CompactionCell {
    fn heading(&self) -> Line<'static> {
        let mut line = Line::from("Context compacted".bold()).fg(Color::Yellow);
        if let Some(elapsed) = self.elapsed {
            line.push_span(
                format!(
                    " · {}",
                    crate::status_indicator_widget::fmt_elapsed_compact(elapsed.as_secs())
                )
                .dim(),
            );
        }
        crate::render::header::paint(line)
    }
}

impl HistoryCell for CompactionCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        adaptive_wrap_lines(vec![self.heading()], RtOptions::new(usize::from(width)))
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![self.heading()]
    }
}

impl FeedEntry for CompactionCell {
    fn feed_kind(&self) -> crate::transcript_view::EntryKind {
        use crate::transcript_view::EntryKind;
        EntryKind::Compaction
    }

    fn feed_id(&self) -> String {
        format!("compaction:{}", self.id)
    }

    fn feed_disclosure(&self) -> HeaderDisclosure {
        HeaderDisclosure::FixedOpen
    }

    fn feed_title(&self) -> Line<'static> {
        self.heading()
    }
}

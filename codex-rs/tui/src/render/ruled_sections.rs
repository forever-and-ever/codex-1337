//! Separator ownership for the lower UI: one rule at each visible section boundary.
use super::renderable::Renderable;
use super::renderable::RenderableItem;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

pub(crate) struct RuledSections<'a>(pub Vec<RenderableItem<'a>>);

pub(crate) struct HorizontalRule;

impl Renderable for HorizontalRule {
    fn desired_height(&self, _width: u16) -> u16 {
        1
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if !area.is_empty() {
            Paragraph::new("─".repeat(usize::from(area.width)).dim())
                .render(Rect { height: 1, ..area }, buf);
        }
    }
}

impl Renderable for RuledSections<'_> {
    fn desired_height(&self, width: u16) -> u16 {
        let heights = self
            .0
            .iter()
            .map(|section| section.desired_height(width))
            .filter(|height| *height > 0);
        let mut total = 0u16;
        for height in heights {
            total = total.saturating_add(height.saturating_add(1));
        }
        total
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() || self.desired_height(area.width) == 0 {
            return;
        }
        let sections: Vec<_> = self
            .0
            .iter()
            .filter(|section| section.desired_height(area.width) > 0)
            .collect();
        if area.height < 2 {
            if let Some(section) = sections.last() {
                section.render(area, buf);
            }
            return;
        }
        let mut y = area.y;
        for (index, section) in sections.iter().enumerate() {
            let height = section.desired_height(area.width);
            // Later sections contain navigation. Give each a row before spending the remaining
            // height on wrapped metadata; omit a section rather than draw adjacent empty rules.
            let reserve =
                u16::try_from((sections.len() - index - 1).saturating_mul(2)).unwrap_or(u16::MAX);
            if area.bottom().saturating_sub(y) <= reserve.saturating_add(1) {
                continue;
            }
            HorizontalRule.render(
                Rect {
                    y,
                    height: 1,
                    ..area
                },
                buf,
            );
            y += 1;
            let height = height.min(area.bottom().saturating_sub(y).saturating_sub(reserve));
            section.render(Rect { y, height, ..area }, buf);
            y += height;
        }
    }
}

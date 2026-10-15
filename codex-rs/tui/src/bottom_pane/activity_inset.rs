use crate::render::live_section;
use crate::render::renderable::FlexRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableItem;
use crate::render::ruled_sections::HorizontalRule;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

pub(super) fn inset<'a>(activity: impl Renderable + 'a) -> RenderableItem<'a> {
    RenderableItem::Owned(Box::new(ActivityInset(activity)))
}

pub(super) fn with_tip<'a>(
    activity: impl Renderable + 'a,
    tip: Option<&'a impl Renderable>,
) -> RenderableItem<'a> {
    let mut group = FlexRenderable::new();
    group.push(/*flex*/ 0, RenderableItem::Owned(Box::new(activity)));
    if let Some(tip) = tip {
        group.push(/*flex*/ 1, RenderableItem::Borrowed(tip));
    }
    inset(group)
}

struct ActivityInset<T>(T);

impl<T: Renderable> Renderable for ActivityInset<T> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let content = live_section::content_area(area);
        let height = self.0.desired_height(content.width);
        // Preserve activity before its trailing blank row and rule on short terminals.
        self.0.render(
            Rect {
                height: height.min(content.height),
                ..content
            },
            buf,
        );
        if area.height > height.saturating_add(1) {
            HorizontalRule.render(
                Rect {
                    y: area.y.saturating_add(height).saturating_add(1),
                    height: 1,
                    ..area
                },
                buf,
            );
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.0
            .desired_height(live_section::content_width(width))
            .saturating_add(2)
    }
}

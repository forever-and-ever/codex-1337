use crate::render::Insets;
use crate::render::RectExt;
use crate::render::renderable::FlexRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableItem;
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
        let area = area.inset(Insets::tlbr(
            /*top*/ 0, /*left*/ 2, /*bottom*/ 0, /*right*/ 0,
        ));
        // Preserve activity before its trailing blank row on short terminals.
        self.0.render(
            Rect {
                height: self.0.desired_height(area.width).min(area.height),
                ..area
            },
            buf,
        );
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.0
            .desired_height(width.saturating_sub(2))
            .saturating_add(1)
    }
}

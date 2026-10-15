use crate::render::Insets;
use crate::render::RectExt;
use crate::render::renderable::FlexRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableItem;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

pub(super) fn inset<'a>(activity: impl Renderable + 'a, trailing_rows: u16) -> RenderableItem<'a> {
    RenderableItem::Owned(Box::new(ActivityInset {
        activity,
        trailing_rows,
    }))
}

pub(super) fn with_tip<'a>(
    activity: impl Renderable + 'a,
    tip: Option<&'a impl Renderable>,
    trailing_rows: u16,
) -> RenderableItem<'a> {
    let mut group = FlexRenderable::new();
    group.push(/*flex*/ 0, RenderableItem::Owned(Box::new(activity)));
    if let Some(tip) = tip {
        group.push(/*flex*/ 1, RenderableItem::Borrowed(tip));
    }
    inset(group, trailing_rows)
}

struct ActivityInset<T> {
    activity: T,
    trailing_rows: u16,
}

impl<T: Renderable> Renderable for ActivityInset<T> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let area = area.inset(Insets::tlbr(
            /*top*/ 0, /*left*/ 2, /*bottom*/ 0, /*right*/ 0,
        ));
        // Preserve activity before its trailing blank row on short terminals.
        self.activity.render(
            Rect {
                height: self.activity.desired_height(area.width).min(area.height),
                ..area
            },
            buf,
        );
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.activity
            .desired_height(width.saturating_sub(2))
            .saturating_add(self.trailing_rows)
    }
}

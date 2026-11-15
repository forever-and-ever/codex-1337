//! Filesystem-backed links are resolved on retained-layout workers, before cached rows are painted.

use super::HistoryCell;

pub(crate) fn prepare_links(cell: &dyn HistoryCell) {
    if let Some(image) = cell.as_any().downcast_ref::<super::ViewImageHistoryCell>() {
        image.link_local_image();
    } else if let Some(item) = cell.as_any().downcast_ref::<super::ToolItemCell>() {
        prepare_links(item.inner.as_ref());
    } else if let Some(group) = cell.as_any().downcast_ref::<super::ToolOutputGroup>() {
        for member in &group.members {
            prepare_links(member.as_ref());
        }
    }
}

//! Explicit status inspection stays visible without opening the surrounding conversation gap.

use super::CompositeHistoryCell;
use super::HistoryCell;
use crate::status::StatusHistoryCell;
use std::sync::Arc;

pub(crate) fn matches(cell: &dyn HistoryCell) -> bool {
    cell.as_any().is::<Arc<StatusHistoryCell>>()
        || cell
            .as_any()
            .downcast_ref::<CompositeHistoryCell>()
            .is_some_and(|cell| cell.parts.iter().any(|part| matches(part.as_ref())))
}

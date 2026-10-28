//! One permission-edit lifecycle owns admission, discovery, choosing and server acceptance.

use super::ChatWidget;

#[derive(Clone, Debug)]
pub(super) enum PermissionIntent {
    Menu,
    Inline(String),
}

#[derive(Default)]
pub(super) enum PermissionPopupState {
    #[default]
    Idle,
    Queued {
        request_id: Option<uuid::Uuid>,
    },
    BackgroundDiscovery {
        request_id: uuid::Uuid,
    },
    Loading {
        request_id: uuid::Uuid,
        intent: PermissionIntent,
    },
    Choosing(PermissionIntent),
    ApplyingInline,
    ApplyingShortcut,
}

impl ChatWidget {
    pub(crate) fn permission_popup_pending(&self) -> bool {
        matches!(self.permission_popup, PermissionPopupState::Queued { .. })
    }

    pub(crate) fn begin_permission_popup(&mut self) -> bool {
        let request_id = match self.permission_popup {
            PermissionPopupState::Idle => None,
            PermissionPopupState::BackgroundDiscovery { request_id } => Some(request_id),
            _ => return false,
        };
        self.permission_popup = PermissionPopupState::Queued { request_id };
        true
    }

    pub(crate) fn permission_popup_dispatched(&mut self) {
        if let PermissionPopupState::Queued { request_id } = self.permission_popup {
            self.permission_popup = request_id.map_or(PermissionPopupState::Idle, |request_id| {
                PermissionPopupState::BackgroundDiscovery { request_id }
            });
        }
    }

    pub(super) fn close_permission_menu(&mut self) {
        match self.permission_popup {
            PermissionPopupState::Loading {
                request_id,
                intent: PermissionIntent::Menu,
            } => {
                self.permission_popup = PermissionPopupState::BackgroundDiscovery { request_id };
            }
            PermissionPopupState::Choosing(PermissionIntent::Menu) => {
                self.permission_popup = PermissionPopupState::Idle;
            }
            _ => {}
        }
    }

    pub(crate) fn permission_popup_request_id(&self) -> Option<uuid::Uuid> {
        match self.permission_popup {
            PermissionPopupState::Loading { request_id, .. }
            | PermissionPopupState::BackgroundDiscovery { request_id } => Some(request_id),
            PermissionPopupState::Queued { request_id } => request_id,
            _ => None,
        }
    }
}

use super::*;
use codex_app_server_protocol::AlarmSummary;
use codex_app_server_protocol::ThreadAlarmsUpdatedNotification;

#[derive(Default)]
pub(super) struct Alarms {
    pub(super) armed: Vec<AlarmSummary>,
    disarming: Option<String>,
}

impl ChatWidget {
    pub(super) fn on_alarms_updated(&mut self, update: ThreadAlarmsUpdatedNotification) {
        if !self
            .thread_id
            .is_some_and(|id| id.to_string() == update.thread_id)
        {
            return;
        }
        self.alarms.armed = update.alarms;
        if self
            .alarms
            .disarming
            .as_ref()
            .is_some_and(|id| !self.alarms.armed.iter().any(|alarm| &alarm.id == id))
        {
            self.alarms.disarming = None;
        }
        self.request_redraw();
    }

    pub(super) fn open_alarms(&mut self) {
        let Some(thread_id) = self.thread_id else {
            self.add_error_message("Session is still starting; try /alarms again shortly.".into());
            return;
        };
        if self.alarms.armed.is_empty() {
            self.add_info_message("No armed alarms.".into(), /*hint*/ None);
            return;
        }
        let items = self
            .alarms
            .armed
            .iter()
            .map(|alarm| {
                let id = alarm.id.clone();
                SelectionItem {
                    name: history_cell::sanitize_user_text(
                        format!(
                            "{} · {}",
                            crate::bottom_pane::alarm_label(alarm),
                            alarm.note
                        )
                        .into(),
                    )
                    .into_owned(),
                    description: Some(
                        history_cell::sanitize_user_text(
                            format!(
                                "{} · {} · disarm",
                                alarm.id,
                                crate::bottom_pane::alarm_detail(alarm)
                            )
                            .into(),
                        )
                        .into_owned(),
                    ),
                    disabled_reason: self
                        .alarms
                        .disarming
                        .as_ref()
                        .map(|_| "Disarming alarm…".into()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::DisarmAlarm {
                            thread_id,
                            id: id.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.show_selection_view(SelectionViewParams {
            title: Some("Armed alarms".into()),
            subtitle: Some("Select an alarm to disarm".into()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..SelectionViewParams::picker()
        });
    }

    pub(crate) fn begin_alarm_disarm(&mut self, thread_id: ThreadId, id: &str) -> bool {
        if self.thread_id != Some(thread_id) || self.alarms.disarming.is_some() {
            return false;
        }
        self.alarms.disarming = Some(id.to_owned());
        self.request_redraw();
        true
    }

    pub(crate) fn alarm_disarm_failed(&mut self, message: String) {
        self.alarms.disarming = None;
        self.add_error_message(message);
    }

    pub(crate) fn cancel_unsent_alarm_disarm(&mut self, id: &str) {
        if self.alarms.disarming.as_deref() == Some(id) {
            self.alarms.disarming = None;
            self.request_redraw();
        }
    }
}

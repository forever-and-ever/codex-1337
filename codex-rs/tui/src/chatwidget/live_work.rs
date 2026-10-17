//! Adapters for the existing alarm, question and agent-name owners.

use super::*;
use crate::live_work::Action;
use crate::live_work::Key;
use crate::live_work::Row;

impl ChatWidget {
    pub(crate) fn open_processes(&mut self) {
        self.app_event_tx.send(AppEvent::LiveWork(
            crate::app::live_work::Event::FocusProcesses,
        ));
    }

    pub(crate) fn live_alarm_rows(&self) -> Vec<Row> {
        let Some(thread_id) = self.thread_id else {
            return Vec::new();
        };
        self.alarms
            .armed
            .iter()
            .map(|alarm| {
                let disarming = self.alarms.disarming.as_ref() == Some(&alarm.id);
                let summary = history_cell::sanitize_user_text(
                    format!(
                        "{}{} · {}",
                        if disarming { "Disarming · " } else { "" },
                        crate::bottom_pane::alarm_label(alarm),
                        alarm.note,
                    )
                    .into(),
                )
                .into_owned();
                let detail = history_cell::sanitize_user_text(
                    format!("{} · {}", alarm.id, crate::bottom_pane::alarm_detail(alarm),).into(),
                )
                .into_owned();
                Row {
                    key: Key::Alarm {
                        thread_id,
                        id: alarm.id.clone(),
                    },
                    summary: summary.into(),
                    details: vec![detail.into()],
                    children: Vec::new(),
                    actions: if self.alarms.disarming.is_none() {
                        vec![Action::DisarmAlarm {
                            thread_id,
                            id: alarm.id.clone(),
                        }]
                    } else {
                        Vec::new()
                    },
                }
            })
            .collect()
    }

    pub(crate) fn live_process_owners(&self) -> HashMap<ThreadId, String> {
        self.subagent_roster
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rows
            .iter()
            .map(|agent| (agent.id, agent.name.clone()))
            .collect()
    }
}

//! Item defaults and individual exceptions share an action clock, never a transcript default.

use crate::history_cell::invocation::Action;
use crate::history_cell::invocation::Panel;
use crate::history_cell::invocation::Port;
use crate::history_cell::output_provenance::OutputMode;
use std::collections::HashMap;

#[derive(Clone, Copy)]
struct Choice<T> {
    revision: u64,
    value: T,
}

fn latest<T: Copy>(left: Option<Choice<T>>, right: Option<Choice<T>>) -> Option<Choice<T>> {
    left.into_iter()
        .chain(right)
        .max_by_key(|choice| choice.revision)
}

#[derive(Clone, Copy, Default)]
struct PortChoices {
    visible: Option<Choice<bool>>,
    mode: Option<Choice<OutputMode>>,
}

impl PortChoices {
    fn merge(self, other: Self) -> Self {
        Self {
            visible: latest(self.visible, other.visible),
            mode: latest(self.mode, other.mode),
        }
    }

    fn panel(self, initial: Panel) -> Panel {
        let mode = self.mode.map_or(initial.mode(), |choice| choice.value);
        if self
            .visible
            .map_or(initial.visible(), |choice| choice.value)
        {
            Panel::Visible(mode)
        } else {
            Panel::Hidden(mode)
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Choices {
    input: PortChoices,
    output: PortChoices,
}

impl Choices {
    fn port(self, port: Port) -> PortChoices {
        match port {
            Port::Input => self.input,
            Port::Output => self.output,
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Policy {
    revision: u64,
    calls: HashMap<String, Choices>,
    items: HashMap<String, Choices>,
}

impl Policy {
    pub(super) fn has_items(&self) -> bool {
        !self.items.is_empty()
    }

    pub(super) fn has_item(&self, item: &str) -> bool {
        self.items.contains_key(item)
    }

    pub(super) fn panel(&self, id: &str, item: Option<&str>, port: Port, initial: Panel) -> Panel {
        let local = self.calls.get(id).copied().unwrap_or_default().port(port);
        let default = item
            .and_then(|item| self.items.get(item))
            .copied()
            .unwrap_or_default()
            .port(port);
        local.merge(default).panel(initial)
    }

    #[expect(
        clippy::expect_used,
        reason = "Exhausting viewer actions violates revision ordering"
    )]
    pub(super) fn apply(&mut self, id: &str, item: Option<&str>, action: Action) {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("port revision exhausted");
        let (port, panel) = match action {
            Action::Toggle(port, panel) => (port, panel),
            Action::Format(port, mode) => (port, Panel::Visible(mode)),
        };
        let choices = if let Some(item) = item {
            self.items.entry(item.to_owned()).or_default()
        } else {
            self.calls.entry(id.to_owned()).or_default()
        };
        let choices = match port {
            Port::Input => &mut choices.input,
            Port::Output => &mut choices.output,
        };
        choices.visible = Some(Choice {
            revision: self.revision,
            value: panel.visible(),
        });
        // Opening a group must not overwrite its members' independent format choices.
        // A single click retains the complete panel hit-tested before any regrouping.
        if item.is_none() || matches!(action, Action::Format(..)) {
            choices.mode = Some(Choice {
                revision: self.revision,
                value: panel.mode(),
            });
        }
    }

    pub(super) fn remap(&mut self, from: &str, to: &str) {
        if from == to {
            return;
        }
        if let Some(moved) = self.items.remove(from) {
            let target = self.items.entry(to.to_owned()).or_default();
            target.input = target.input.merge(moved.input);
            target.output = target.output.merge(moved.output);
        }
    }
}

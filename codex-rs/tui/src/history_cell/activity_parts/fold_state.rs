//! Viewer-owned choices. A group action supersedes older exceptions without visiting history.

use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FoldGroup {
    CodeMode,
    Command,
    Response,
    Message,
    Injection,
    User,
    Preamble,
    Alarm,
    Compaction,
    Farm,
    Status,
    Diff,
    Result,
    RawContext,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum FoldScope {
    #[default]
    Transcript,
    Item(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Choice {
    revision: u64,
    open: bool,
}

/// Worker snapshots share immutable policy; only viewer actions copy retained choices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FoldState {
    policy: Arc<FoldPolicy>,
    revision: u64,
    scope: FoldScope,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FoldPolicy {
    choices: HashMap<String, Choice>,
    defaults: HashMap<FoldScope, HashMap<FoldGroup, Choice>>,
}

impl<const N: usize> From<[String; N]> for FoldState {
    fn from(ids: [String; N]) -> Self {
        ids.into_iter().collect()
    }
}

impl FoldState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn has_item_defaults(&self) -> bool {
        self.policy
            .defaults
            .keys()
            .any(|scope| matches!(scope, FoldScope::Item(_)))
    }

    pub(crate) fn scoped(&self, scope: FoldScope) -> Self {
        Self {
            scope,
            ..self.clone()
        }
    }

    pub(crate) fn item_scope(&self) -> Option<&str> {
        match &self.scope {
            FoldScope::Item(item) => Some(item),
            FoldScope::Transcript => None,
        }
    }

    pub(crate) fn is_open(&self, group: FoldGroup, id: &str, initial: bool) -> bool {
        self.is_open_in(&self.scope, group, id, initial)
    }

    pub(crate) fn is_open_in(
        &self,
        scope: &FoldScope,
        group: FoldGroup,
        id: &str,
        initial: bool,
    ) -> bool {
        let default = self
            .policy
            .defaults
            .get(scope)
            .and_then(|groups| groups.get(&group));
        self.policy
            .choices
            .get(id)
            .filter(|choice| default.is_none_or(|default| choice.revision > default.revision))
            .or(default)
            .map_or(initial, |choice| choice.open)
    }

    pub(crate) fn set_default(&mut self, scope: FoldScope, group: FoldGroup, open: bool) {
        let choice = self.choice(open);
        Arc::make_mut(&mut self.policy)
            .defaults
            .entry(scope)
            .or_default()
            .insert(group, choice);
    }

    pub(crate) fn set(&mut self, id: String, open: bool) {
        let choice = self.choice(open);
        Arc::make_mut(&mut self.policy).choices.insert(id, choice);
    }

    pub(crate) fn forget(&mut self, id: &str) {
        if self.policy.choices.contains_key(id) {
            Arc::make_mut(&mut self.policy).choices.remove(id);
        }
    }
    pub(crate) fn remap(&mut self, from: &str, to: &str) {
        if from == to || !self.policy.choices.contains_key(from) {
            return;
        }
        let choices = &mut Arc::make_mut(&mut self.policy).choices;
        if let Some(choice) = choices.remove(from) {
            let current = choices.entry(to.to_owned()).or_insert(choice);
            if choice.revision > current.revision {
                *current = choice;
            }
        }
    }
    pub(crate) fn remap_scope(&mut self, from: &FoldScope, to: FoldScope) {
        if *from == to || !self.policy.defaults.contains_key(from) {
            return;
        }
        let defaults = &mut Arc::make_mut(&mut self.policy).defaults;
        if let Some(moved) = defaults.remove(from) {
            let target = defaults.entry(to).or_default();
            for (group, choice) in moved {
                let current = target.entry(group).or_insert(choice);
                if choice.revision > current.revision {
                    *current = choice;
                }
            }
        }
    }

    // Ungrouped controls (notably [cot]) retain their existing independent choices.
    pub(crate) fn contains(&self, id: &str) -> bool {
        self.policy
            .choices
            .get(id)
            .is_some_and(|choice| choice.open)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &String> {
        self.policy
            .choices
            .iter()
            .filter(|(_, choice)| choice.open)
            .map(|(id, _)| id)
    }

    pub(crate) fn insert(&mut self, id: String) -> bool {
        let changed = !self.contains(&id);
        self.set(id, true);
        changed
    }

    pub(crate) fn remove(&mut self, id: &str) -> bool {
        let open = self.contains(id);
        self.set(id.to_owned(), false);
        open
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&String) -> bool) {
        let closed: Vec<_> = self
            .policy
            .choices
            .iter()
            .filter(|(id, choice)| choice.open && !keep(id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in closed {
            self.set(id, false);
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "Exhausting viewer actions violates revision ordering"
    )]
    fn choice(&mut self, open: bool) -> Choice {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("fold revision exhausted");
        Choice {
            revision: self.revision,
            open,
        }
    }
}

impl Extend<String> for FoldState {
    fn extend<T: IntoIterator<Item = String>>(&mut self, ids: T) {
        for id in ids {
            if !self.contains(&id) {
                self.insert(id);
            }
        }
    }
}

impl FromIterator<String> for FoldState {
    fn from_iter<T: IntoIterator<Item = String>>(ids: T) -> Self {
        let mut state = Self::new();
        state.extend(ids);
        state
    }
}

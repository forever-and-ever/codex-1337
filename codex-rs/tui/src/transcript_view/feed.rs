//! The Activity Feed: one container per step, folded by default.
//!
//! The feed is a presentation over the retained cells, never a second transcript. Each cell
//! the server billed belongs to its step's Feed Item: the step's Thought, the response's
//! messages and calls, and their results. Steps that share a cell, and a Code Mode exec with
//! the steps that wait on it, form one item. User turns, arrivals and Preambles are items of
//! one cell each. Every other cell stays loose and draws as before.
//!
//! An item's members need not be adjacent. A run of members that no other item interrupts is
//! a segment, and each segment draws a header above its first visible member: clock, fold,
//! title, then the figures of the steps that begin in it. Folded, a segment shows only its
//! header, and the messages of an item that made calls. Unfolded, it shows every member, and
//! an item of several steps heads each step's first member with that step's own figures; the
//! one fold drives both levels. User turns and responses unfold by default, live work while
//! it lasts, and an explicit fold or unfold outlasts both.
//!
//! Live work stays below a rule: the trailing items still in flight, else a running tool in
//! the live tail.

use super::step_bills::Figures;
use super::step_bills::StepBills;
use super::step_bills::Total;
use super::step_bills::arrival_figures;
use super::step_bills::steps_figures;
use super::step_bills::user_figures;
use super::*;
use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::ArrivalCell;
use crate::history_cell::CodeModeCell;
use crate::history_cell::CodeModeFactCell;
use crate::history_cell::CodeModeWaitCell;
use crate::history_cell::HistoryTimestamp;
use crate::history_cell::PreambleCell;
use crate::history_cell::ThoughtCell;
use crate::history_cell::UserHistoryCell;
use crate::history_cell::activity_parts::FoldTarget;
use crate::line_truncation::line_width;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use codex_app_server_protocol::StepAdmissionItem;
use codex_protocol::code_mode::CodeModeEvent;
use ratatui::text::Span;
use std::collections::HashMap;
use std::collections::HashSet;

/// A cell that is a Feed Item of its own: its header carries the title and its fold the body.
pub(crate) trait FeedEntry {
    fn feed_title(&self) -> Line<'static>;
    /// The body for a column `width` wide, with its own indentation.
    fn feed_body(&self, width: u16) -> Vec<HyperlinkLine>;
}

fn feed_entry(cell: &dyn HistoryCell) -> Option<&dyn FeedEntry> {
    let any = cell.as_any();
    if let Some(arrival) = any.downcast_ref::<ArrivalCell>() {
        return Some(arrival);
    }
    any.downcast_ref::<PreambleCell>()
        .map(|preamble| preamble as &dyn FeedEntry)
}

/// What unfolding a feed entry shows under its header.
pub(super) fn entry_body(cell: &dyn HistoryCell, width: u16) -> Vec<HyperlinkLine> {
    feed_entry(cell).map_or_else(Vec::new, |entry| entry.feed_body(width))
}

#[derive(Default)]
pub(super) struct FeedIndex {
    /// The layout frame in which every cell was last verified against this index.
    frame: Option<u64>,
    cells: Vec<usize>,
    bills: u64,
    /// Advances on every rebuild; layouts derived from a seat carry it.
    pub(super) revision: u64,
    roles: Vec<Role>,
    items: Vec<Item>,
    folds: HashMap<String, usize>,
    /// How many of each step's user inputs the feed placed on users' own items.
    placed_users: HashMap<String, usize>,
    live_from: Option<usize>,
}

#[derive(Clone, Default)]
enum Role {
    #[default]
    Loose,
    Member(Membership),
}

#[derive(Clone)]
struct Membership {
    item: usize,
    segment: usize,
    /// The item's steps this cell belongs to, as indices into its steps.
    steps: Vec<usize>,
    /// The steps whose first member this is.
    opens: Vec<usize>,
    message: bool,
}

struct Item {
    kind: Kind,
    fold: String,
    unfold_mark: String,
    fold_mark: String,
    steps: Vec<String>,
    segments: Vec<Vec<usize>>,
    live: bool,
}

#[derive(Clone, PartialEq, Eq)]
enum Kind {
    /// One or more steps; `calls` when any of them made a call.
    Steps {
        calls: bool,
    },
    /// The step whose seal admitted the input, and the input's place among its user inputs.
    User {
        admitted: Option<(String, usize)>,
    },
    Arrival,
    Preamble,
}

impl Item {
    fn unfolded(&self, expanded: &HashSet<String>) -> bool {
        if expanded.contains(&self.unfold_mark) {
            true
        } else if expanded.contains(&self.fold_mark) {
            false
        } else {
            self.live || self.spacious()
        }
    }

    /// User turns and responses stand apart and unfold by default.
    fn spacious(&self) -> bool {
        matches!(self.kind, Kind::User { .. } | Kind::Steps { calls: false })
    }
}

/// How one cell draws in the feed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Seat {
    pub(super) header: Option<Header>,
    pub(super) content: Content,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Content {
    /// Not part of the feed: drawn as before, with its own clock.
    Loose,
    Hidden,
    /// The cell's own rows, under its item's header.
    Own,
    /// A feed entry's body.
    Body,
}

/// A header row above a member.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Header {
    item: usize,
    /// The members its title and clock come from.
    scope: Vec<usize>,
    /// The item's steps it bills.
    steps: Vec<usize>,
    pub(super) unfolded: bool,
    pub(super) spacious: bool,
}

impl Seat {
    pub(super) const LOOSE: Self = Self {
        header: None,
        content: Content::Loose,
    };

    pub(super) fn is_visible(&self) -> bool {
        self.header.is_some() || self.content != Content::Hidden
    }
}

impl FeedIndex {
    /// Whether the index still describes these cells and bills.
    fn verify(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        frame: u64,
        bills: u64,
    ) -> bool {
        if self.bills != bills || self.cells.len() != cells.len() {
            return false;
        }
        if self.cells.get(index) != cells.get(index).map(cell_key).as_ref() {
            return false;
        }
        if self.frame != Some(frame) {
            if !self.cells.iter().copied().eq(cells.iter().map(cell_key)) {
                return false;
            }
            self.frame = Some(frame);
        }
        true
    }

    fn rebuild<'b>(&mut self, cells: &[Arc<dyn HistoryCell>], bills: &'b StepBills, frame: u64) {
        let mut names: Vec<&str> = Vec::new();
        let mut nodes: HashMap<&str, usize> = HashMap::new();
        let mut parent: Vec<usize> = Vec::new();
        let mut cell_nodes: Vec<Vec<usize>> = Vec::with_capacity(cells.len());
        for cell in cells {
            let (steps, link) = steps_of(cell.as_ref(), bills);
            let mut intern = |step: &'b str| -> usize {
                *nodes.entry(step).or_insert_with(|| {
                    names.push(step);
                    parent.push(parent.len());
                    parent.len() - 1
                })
            };
            let own: Vec<usize> = steps.into_iter().map(&mut intern).collect();
            let linked = link.map(&mut intern);
            if let Some(first) = own.first() {
                for node in own.iter().skip(1).chain(linked.as_ref()) {
                    let (left, right) = (find(&mut parent, *first), find(&mut parent, *node));
                    parent[right] = left;
                }
            }
            cell_nodes.push(own);
        }

        let mut roles = vec![Role::Loose; cells.len()];
        let mut items: Vec<Item> = Vec::new();
        let mut item_of_root: HashMap<usize, usize> = HashMap::new();
        let mut placed_users: HashMap<String, usize> = HashMap::new();
        let mut last_item = None;
        let mut last_step: Option<&str> = None;
        for (index, cell) in cells.iter().enumerate() {
            if let Some((kind, key)) = entry_kind(cell, last_step, &mut placed_users) {
                items.push(Item::new(kind, key));
                let item = items.len() - 1;
                items[item].segments.push(vec![index]);
                roles[index] = Role::Member(Membership {
                    item,
                    segment: 0,
                    steps: Vec::new(),
                    opens: Vec::new(),
                    message: false,
                });
                last_item = Some(item);
                continue;
            }
            let Some(first) = cell_nodes[index].first() else {
                continue;
            };
            let root = find(&mut parent, *first);
            let item = *item_of_root.entry(root).or_insert_with(|| {
                items.push(Item::new(
                    Kind::Steps { calls: false },
                    names[*first].to_string(),
                ));
                items.len() - 1
            });
            if last_item != Some(item) || items[item].segments.is_empty() {
                items[item].segments.push(Vec::new());
            }
            let segment = items[item].segments.len() - 1;
            items[item].segments[segment].push(index);
            let mut steps = Vec::new();
            let mut opens = Vec::new();
            for node in &cell_nodes[index] {
                let name = names[*node];
                let position = match items[item].steps.iter().position(|step| step == name) {
                    Some(position) => position,
                    None => {
                        items[item].steps.push(name.to_string());
                        opens.push(items[item].steps.len() - 1);
                        items[item].steps.len() - 1
                    }
                };
                steps.push(position);
            }
            if let Some(node) = cell_nodes[index].last() {
                last_step = Some(names[*node]);
            }
            roles[index] = Role::Member(Membership {
                item,
                segment,
                steps,
                opens,
                message: cell.as_any().is::<AgentMarkdownCell>(),
            });
            last_item = Some(item);
        }

        for item in &mut items {
            if let Kind::Steps { calls } = &mut item.kind {
                *calls = item.steps.iter().any(|step| {
                    bills
                        .step(step)
                        .is_some_and(|step| !step.call_ids.is_empty())
                });
            }
            item.live = item
                .segments
                .iter()
                .flatten()
                .any(|index| in_flight(cells[*index].as_ref()));
        }

        // Live work trails the transcript: the segments of items in flight, and loose
        // cells in flight, with whatever lies between their members.
        let mut live = vec![false; cells.len()];
        for item in items.iter().filter(|item| item.live) {
            for segment in &item.segments {
                if let (Some(first), Some(last)) = (segment.first(), segment.last()) {
                    live[*first..=*last].fill(true);
                }
            }
        }
        for (index, cell) in cells.iter().enumerate() {
            live[index] |= matches!(roles[index], Role::Loose) && in_flight(cell.as_ref());
        }
        let trailing = live.iter().rev().take_while(|live| **live).count();

        self.folds = items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.fold.clone(), index))
            .collect();
        self.live_from = (trailing > 0).then(|| cells.len() - trailing);
        self.cells = cells.iter().map(cell_key).collect();
        self.frame = Some(frame);
        self.bills = bills.revision();
        self.revision = self.revision.wrapping_add(/*rhs*/ 1);
        self.roles = roles;
        self.items = items;
        self.placed_users = placed_users;
    }

    pub(super) fn seat(
        &self,
        index: usize,
        expanded: &HashSet<String>,
        elided: &dyn Fn(usize) -> bool,
    ) -> Seat {
        let Some(Role::Member(member)) = self.roles.get(index) else {
            return Seat::LOOSE;
        };
        let item = &self.items[member.item];
        let unfolded = item.unfolded(expanded);
        let spacious = item.spacious();
        let segment = &item.segments[member.segment];
        let header = |scope: Vec<usize>, steps: Vec<usize>| Header {
            item: member.item,
            scope,
            steps,
            unfolded,
            spacious,
        };
        match item.kind {
            Kind::User { .. } | Kind::Arrival | Kind::Preamble => Seat {
                header: Some(header(vec![index], Vec::new())),
                content: match (&item.kind, unfolded) {
                    (_, false) => Content::Hidden,
                    (Kind::User { .. }, true) => Content::Own,
                    (_, true) => Content::Body,
                },
            },
            Kind::Steps { calls } => {
                let content = if unfolded || (calls && member.message) {
                    Content::Own
                } else {
                    Content::Hidden
                };
                let header = if unfolded && item.steps.len() > 1 {
                    // Each step heads its own first visible member; the segment's first visible
                    // member is headed even when its steps began in an earlier segment.
                    let mut pending = Vec::new();
                    let mut first = true;
                    let mut found = None;
                    for candidate in segment {
                        if let Some(Role::Member(other)) = self.roles.get(*candidate) {
                            pending.extend(other.opens.iter().copied());
                        }
                        if elided(*candidate) {
                            continue;
                        }
                        let headed = first || !pending.is_empty();
                        first = false;
                        let steps = std::mem::take(&mut pending);
                        if *candidate == index {
                            found = headed.then(|| {
                                let scope = if steps.is_empty() {
                                    segment.clone()
                                } else {
                                    self.step_members(segment, &steps)
                                };
                                header(scope, steps)
                            });
                            break;
                        }
                    }
                    found
                } else {
                    segment
                        .iter()
                        .copied()
                        .find(|candidate| !elided(*candidate))
                        .filter(|site| *site == index)
                        .map(|_| header(segment.clone(), self.segment_steps(segment)))
                };
                Seat { header, content }
            }
        }
    }

    fn segment_steps(&self, segment: &[usize]) -> Vec<usize> {
        segment
            .iter()
            .filter_map(|index| match self.roles.get(*index) {
                Some(Role::Member(member)) => Some(member.opens.iter().copied()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn step_members(&self, segment: &[usize], steps: &[usize]) -> Vec<usize> {
        segment
            .iter()
            .copied()
            .filter(|index| {
                matches!(self.roles.get(*index), Some(Role::Member(member))
                    if member.steps.iter().any(|step| steps.contains(step)))
            })
            .collect()
    }

    /// Whether the cell belongs to an item drawn as a compact list.
    pub(super) fn compact(&self, index: usize) -> bool {
        matches!(self.roles.get(index), Some(Role::Member(member))
            if !self.items[member.item].spacious())
    }

    pub(super) fn live_from(&self) -> Option<usize> {
        self.live_from
    }

    pub(super) fn fold_target(&self, header: &Header) -> FoldTarget {
        FoldTarget::Part(self.items[header.item].fold.clone())
    }

    /// The fold this cell's header drives, if it is a member.
    pub(super) fn fold_of(&self, index: usize) -> Option<&str> {
        match self.roles.get(index) {
            Some(Role::Member(member)) => Some(self.items[member.item].fold.as_str()),
            _ => None,
        }
    }

    pub(super) fn is_unfolded(&self, fold: &str, expanded: &HashSet<String>) -> Option<bool> {
        let item = self.folds.get(fold)?;
        Some(self.items[*item].unfolded(expanded))
    }

    /// Record the opposite of the item's current state as an explicit choice.
    pub(super) fn toggle(&self, fold: &str, expanded: &mut HashSet<String>) -> Option<usize> {
        let index = *self.folds.get(fold)?;
        let item = &self.items[index];
        let unfolded = item.unfolded(expanded);
        expanded.remove(&item.unfold_mark);
        expanded.remove(&item.fold_mark);
        expanded.insert(if unfolded {
            item.fold_mark.clone()
        } else {
            item.unfold_mark.clone()
        });
        Some(index)
    }

    /// The header that remains for `near`'s segment of the item, or its first segment's.
    pub(super) fn header_site(
        &self,
        item: usize,
        near: usize,
        elided: &dyn Fn(usize) -> bool,
    ) -> Option<usize> {
        let segment = match self.roles.get(near) {
            Some(Role::Member(member)) if member.item == item => {
                &self.items[item].segments[member.segment]
            }
            _ => self.items.get(item)?.segments.first()?,
        };
        segment.iter().copied().find(|index| !elided(*index))
    }

    pub(super) fn header_line(
        &self,
        cells: &[Arc<dyn HistoryCell>],
        header: &Header,
        bills: &StepBills,
        width: u16,
    ) -> Line<'static> {
        use crate::transcript_geometry::CLOCK_COLUMNS;
        use crate::transcript_geometry::TITLE_COLUMN;
        let item = &self.items[header.item];
        // A replayed member may carry no clock; its step's completion still dates it.
        let clock = header
            .scope
            .iter()
            .find_map(|index| cells.get(*index)?.transcript_timestamp())
            .or_else(|| {
                header.steps.iter().find_map(|step| {
                    HistoryTimestamp::from_millis(bills.step(&item.steps[*step])?.completed_at_ms)
                })
            });
        let mut spans = vec![
            clock.map_or_else(
                || " ".repeat(usize::from(CLOCK_COLUMNS)).into(),
                HistoryTimestamp::span,
            ),
            " ".repeat(usize::from(TITLE_COLUMN - CLOCK_COLUMNS)).into(),
        ];
        let title_width = usize::from(width.saturating_sub(/*rhs*/ 44).clamp(16, 48));
        let title =
            truncate_line_with_ellipsis_if_overflow(self.title(cells, header, width), title_width);
        let figures = self.figures(cells, header, bills);
        let used = line_width(&title);
        spans.extend(title.spans);
        if let Some(figures) = figures {
            spans.push(" ".repeat(title_width - used.min(title_width) + 1).into());
            spans.push(total_span(figures.total));
            if let Some(split) = figures.split {
                spans.push(format!(" {split}").dim());
            }
            if !figures.notes.is_empty() {
                spans.push(format!("  {}", figures.notes.join(" · ")).dim());
            }
        }
        Line::from(spans)
    }

    fn title(&self, cells: &[Arc<dyn HistoryCell>], header: &Header, width: u16) -> Line<'static> {
        let item = &self.items[header.item];
        let first = header.scope.first().and_then(|index| cells.get(*index));
        match &item.kind {
            Kind::User { .. } => return Line::from("You".bold()),
            Kind::Arrival | Kind::Preamble => {
                if let Some(entry) = first.and_then(|cell| feed_entry(cell.as_ref())) {
                    return entry.feed_title();
                }
            }
            Kind::Steps { .. } => {}
        }
        let members = || header.scope.iter().filter_map(|index| cells.get(*index));
        let content_width =
            crate::transcript_geometry::content_width(width, HistoryRenderMode::Rich);
        if let Some(line) = members()
            .find(|cell| {
                !cell.as_any().is::<ThoughtCell>() && !cell.as_any().is::<AgentMarkdownCell>()
            })
            .and_then(|cell| {
                cell.display_lines(content_width)
                    .into_iter()
                    .find(|line| line_width(line) > 0)
            })
        {
            return strip_marker(line);
        }
        let label = if members().any(|cell| cell.as_any().is::<AgentMarkdownCell>()) {
            "Response"
        } else if members().any(|cell| cell.as_any().is::<ThoughtCell>()) {
            "Thought"
        } else {
            "Step"
        };
        Line::from(label.bold())
    }

    fn figures(
        &self,
        cells: &[Arc<dyn HistoryCell>],
        header: &Header,
        bills: &StepBills,
    ) -> Option<Figures> {
        let item = &self.items[header.item];
        let cell = header.scope.first().and_then(|index| cells.get(*index));
        let figures = match &item.kind {
            Kind::Steps { .. } => {
                if header.steps.is_empty() {
                    return None;
                }
                let steps: Vec<&StepAdmissionItem> = header
                    .steps
                    .iter()
                    .filter_map(|step| bills.step(&item.steps[*step]))
                    .collect();
                steps_figures(&steps, &|response| {
                    self.placed_users.get(response).copied().unwrap_or(0)
                })
            }
            Kind::User { admitted } => {
                let (step, ordinal) = admitted.as_ref()?;
                user_figures(bills.step(step), *ordinal)
            }
            Kind::Arrival => {
                let arrival = cell?.as_any().downcast_ref::<ArrivalCell>()?;
                arrival_figures(bills.step(arrival.response_id()), arrival.id())
            }
            Kind::Preamble => Figures {
                total: Total::Exact(cell?.activity_header_tokens()?),
                ..Figures::default()
            },
        };
        (figures.total != Total::Unknown).then_some(figures)
    }
}

impl Item {
    fn new(kind: Kind, key: String) -> Self {
        let fold = format!("feed:{key}");
        Self {
            kind,
            unfold_mark: format!("{fold}+"),
            fold_mark: format!("{fold}-"),
            fold,
            steps: Vec::new(),
            segments: Vec::new(),
            live: false,
        }
    }
}

fn cell_key(cell: &Arc<dyn HistoryCell>) -> usize {
    Arc::as_ptr(cell).cast::<()>() as usize
}

fn find(parent: &mut [usize], mut node: usize) -> usize {
    while parent[node] != node {
        parent[node] = parent[parent[node]];
        node = parent[node];
    }
    node
}

/// Whether the cell is still running: animating, or a Code Mode block that has not finished.
fn in_flight(cell: &dyn HistoryCell) -> bool {
    cell.transcript_animation_tick().is_some()
        || cell
            .as_any()
            .downcast_ref::<CodeModeCell>()
            .is_some_and(CodeModeCell::is_active)
}

/// The user turns, arrivals and Preambles that are items of their own, with their fold keys.
fn entry_kind(
    cell: &Arc<dyn HistoryCell>,
    last_step: Option<&str>,
    placed_users: &mut HashMap<String, usize>,
) -> Option<(Kind, String)> {
    let any = cell.as_any();
    if any.is::<UserHistoryCell>() {
        let admitted = last_step.map(|step| {
            let placed = placed_users.entry(step.to_string()).or_default();
            *placed += 1;
            (step.to_string(), *placed - 1)
        });
        return Some((
            Kind::User { admitted },
            format!("user:{:x}", cell_key(cell)),
        ));
    }
    if let Some(arrival) = any.downcast_ref::<ArrivalCell>() {
        return Some((Kind::Arrival, arrival.id().to_string()));
    }
    if any.is::<PreambleCell>() {
        let id = cell.activity_ids().into_iter().next()?;
        return Some((Kind::Preamble, id));
    }
    None
}

/// The billed steps a cell belongs to, and a step it joins without belonging to it: a Code
/// Mode wait joins its exec's item.
fn steps_of<'b>(cell: &dyn HistoryCell, bills: &'b StepBills) -> (Vec<&'b str>, Option<&'b str>) {
    let any = cell.as_any();
    let known = |response: &str| bills.step(response).map(|step| step.response_id.as_str());
    if let Some(thought) = any.downcast_ref::<ThoughtCell>() {
        return (known(thought.response_id()).into_iter().collect(), None);
    }
    if let Some(message) = any.downcast_ref::<AgentMarkdownCell>() {
        let step = message
            .item_id
            .as_deref()
            .and_then(|id| bills.step_of_message(id));
        return (step.into_iter().collect(), None);
    }
    if let Some(block) = any.downcast_ref::<CodeModeCell>() {
        return (
            bills
                .step_of_call(&block.exec_call_id)
                .into_iter()
                .collect(),
            None,
        );
    }
    if let Some((call, exec)) = wait_calls(cell) {
        let link = bills.step_of_call(exec);
        return match bills.step_of_call(call) {
            Some(step) => (vec![step], link.filter(|link| *link != step)),
            None => (link.into_iter().collect(), None),
        };
    }
    let mut calls = cell.tool_call_ids();
    if calls.is_empty() {
        calls.extend(cell.outer_call_id());
    }
    let mut steps: Vec<&str> = Vec::new();
    for call in calls {
        if let Some(step) = bills.step_of_call(call)
            && !steps.contains(&step)
        {
            steps.push(step);
        }
    }
    (steps, None)
}

/// A Code Mode wait's own call and the exec it waits on.
fn wait_calls(cell: &dyn HistoryCell) -> Option<(&str, &str)> {
    let any = cell.as_any();
    let (fact, exec) = if let Some(wait) = any.downcast_ref::<CodeModeWaitCell>() {
        (&wait.0.0, wait.1.exec_call_id.as_str())
    } else {
        let fact = &any.downcast_ref::<CodeModeFactCell>()?.0;
        (fact, fact.exec_call_id.as_str())
    };
    match &fact.event {
        CodeModeEvent::WaitStarted { call_id } | CodeModeEvent::WaitCompleted { call_id } => {
            Some((call_id.as_str(), exec))
        }
        _ => None,
    }
}

/// Drop a row's leading marker: a bullet or spinner glyph and the space after it.
fn strip_marker(line: Line<'static>) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len());
    let mut leading = true;
    let mut marker = false;
    for mut span in line.spans {
        if leading {
            let content = span.content.trim_start();
            if content.is_empty() {
                continue;
            }
            let mut chars = content.chars();
            if !marker
                && let Some(glyph) = chars.next()
                && !glyph.is_alphanumeric()
                && chars
                    .as_str()
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
            {
                marker = true;
                let rest = chars.as_str().trim_start();
                if rest.is_empty() {
                    continue;
                }
                span.content = rest.to_string().into();
            } else {
                span.content = content.to_string().into();
            }
            leading = false;
        }
        spans.push(span);
    }
    Line::from(spans).style(line.style)
}

/// The total, right-aligned and colored by size; placeholders stay dim.
fn total_span(total: Total) -> Span<'static> {
    const WIDTH: usize = 6;
    match total {
        Total::Exact(tokens) => super::text::token_figure(String::new(), tokens, WIDTH),
        Total::Approximate(tokens) => super::text::token_figure("≈".into(), tokens, WIDTH),
        Total::Pending => format!("{:>WIDTH$}", "…").dim(),
        Total::Severed => format!("{:>WIDTH$}", "—").dim(),
        Total::Unknown => " ".repeat(WIDTH).into(),
    }
}

impl TranscriptView {
    pub(crate) fn record_step_admission(&mut self, item: StepAdmissionItem) {
        if self.step_bills.record(item) {
            self.cache.clear();
            self.live_key = None;
        }
    }

    /// The cell's place in the feed; loose wherever the feed is not drawn.
    pub(super) fn feed_seat(&mut self, cells: &[Arc<dyn HistoryCell>], index: usize) -> Seat {
        if self.detailed || self.mode != HistoryRenderMode::Rich || index >= cells.len() {
            return Seat::LOOSE;
        }
        self.refresh_feed(cells, index);
        self.feed.seat(index, &self.disclosure.expanded, &|index| {
            self.elides_cell(cells, index)
        })
    }

    /// The seat as last indexed, without refreshing; loose if the index is stale.
    pub(super) fn indexed_seat(&self, cells: &[Arc<dyn HistoryCell>], index: usize) -> Seat {
        if self.detailed
            || self.mode != HistoryRenderMode::Rich
            || self.feed.cells.len() != cells.len()
            || self.feed.cells.get(index) != cells.get(index).map(cell_key).as_ref()
        {
            return Seat::LOOSE;
        }
        self.feed.seat(index, &self.disclosure.expanded, &|index| {
            self.elides_cell(cells, index)
        })
    }

    pub(super) fn refresh_feed(&mut self, cells: &[Arc<dyn HistoryCell>], index: usize) {
        let frame = self.cache.frame();
        if !self
            .feed
            .verify(cells, index, frame, self.step_bills.revision())
        {
            self.feed.rebuild(cells, &self.step_bills, frame);
        }
    }

    /// Whether the rule above live work belongs on this entry: the first visible entry of
    /// the trailing work in flight, else a running tool in the live tail.
    pub(super) fn feed_rule(&mut self, cells: &[Arc<dyn HistoryCell>], index: usize) -> bool {
        if self.detailed || self.mode != HistoryRenderMode::Rich {
            return false;
        }
        if let Some(last) = cells.len().checked_sub(/*rhs*/ 1) {
            self.refresh_feed(cells, last);
        }
        let start = self
            .feed
            .live_from()
            .or_else(|| (self.live.is_some() && self.live_tool_entry).then_some(cells.len()));
        let Some(start) = start.filter(|start| *start <= index) else {
            return false;
        };
        (start..index).all(|earlier| {
            self.elides_cell(cells, earlier) || !self.feed_seat(cells, earlier).is_visible()
        })
    }

    /// Fold or unfold a Feed Item, keeping its remaining header in view and focused.
    pub(super) fn toggle_feed(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        fold: &str,
    ) -> bool {
        if let Some(last) = cells.len().checked_sub(/*rhs*/ 1) {
            self.refresh_feed(cells, last);
        }
        let Some(item) = self.feed.toggle(fold, &mut self.disclosure.expanded) else {
            return false;
        };
        self.end_selection(cells);
        self.release_live_reading();
        self.cache.clear();
        self.live_key = None;
        self.live_separated = None;
        self.live_ruled = None;
        let site = self
            .feed
            .header_site(item, index, &|index| self.elides_cell(cells, index))
            .unwrap_or(index);
        let target = FoldTarget::Part(fold.to_string());
        let row = self
            .current_layout(cells, site)
            .and_then(|layout| {
                let (row, _, _) = layout.fold_position(&target)?;
                Some(layout.row_for_offset(/*offset*/ 0) as isize - row as isize)
            })
            .unwrap_or(0);
        let header_visible = self
            .visible
            .iter()
            .any(|visible| visible.index == site && visible.key == EntryKey::cell(&cells[site]));
        if !header_visible || site != index {
            self.position = Position::Reading(Anchor {
                key: EntryKey::cell(&cells[site]),
                index: site,
                offset: 0,
                row_bias: row,
            });
        }
        self.disclosure.focused = Some(super::disclosure::ActivityFocus {
            ids: self.activity_ids(cells, site),
            target,
        });
        self.restart_search();
        true
    }
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod tests;

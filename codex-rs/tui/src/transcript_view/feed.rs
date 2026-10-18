//! The Activity Feed: one container per step, unfolded until the reader folds it.
//!
//! The feed is a presentation over the retained cells, never a second transcript. Each cell
//! the server billed belongs to its step's Feed Item: the step's Thought, the response's
//! messages and calls, and their results. Steps that share a cell, and a Code Mode exec with
//! the steps that wait on it, form one item. User turns, arrivals and Preambles are items of
//! one cell each, and so is a finished reply no step claims, from a chat older than its
//! bills. Every other cell stays loose, with its own clock and tokens.
//!
//! A Thought opens its step. The server announces it only when its response completes, after
//! the response's messages and calls, so the transcript rotates it ahead of them once the
//! step's bill names them.
//!
//! An item's members need not be adjacent. A run of members that no other item interrupts is
//! a segment, and a segment draws a header when it begins a step or shows a member: clock,
//! fold, title, then the figures of the steps that begin in it. The title is the first row of
//! the segment's first activity, which yields that row to it: folded, the header clips it to
//! one row; unfolded, it wraps it whole, and the activity draws the rest. An activity left
//! without a row draws the first row of its details, which holds the control that shows them.
//! Invocation headers carry independent source and return ports; file edits keep their folds.
//! A segment without an activity borrows the item's title, and an item without any is a
//! Response. Folded, a segment shows only its header, and the messages of an item that made
//! calls. Unfolded, it shows every member, and an item of several steps heads each step's first
//! member with that step's own figures; the one fold drives both levels. A Thought shows only
//! when it has text to read: otherwise its tokens are its step's thought figure.

use super::step_bills::Figures;
use super::step_bills::Own;
use super::step_bills::Split;
use super::step_bills::StepBills;
use super::step_bills::Total;
use super::step_bills::arrival_figures;
use super::step_bills::steps_figures;
use super::step_bills::user_figures;
use super::text::HeaderControl;
use super::text::HeaderRows;
use super::text::token_figure;
use super::text::token_label;
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
use crate::history_cell::activity_parts::ActivityParts;
use crate::history_cell::activity_parts::FoldTarget;
use crate::history_cell::activity_preview::compact_header;
use crate::history_cell::output_provenance::OutputMode;
use crate::line_truncation::line_width;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use codex_app_server_protocol::StepAdmissionItem;
use codex_protocol::code_mode::CodeModeEvent;
use ratatui::text::Span;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::Range;

/// A cell that is a Feed Item of its own: its header carries the title and its fold the body.
pub(crate) trait FeedEntry {
    fn feed_id(&self) -> String;
    fn feed_disclosure(&self) -> HeaderDisclosure {
        HeaderDisclosure::Expanded
    }
    fn feed_title(&self) -> Line<'static>;
    /// Body rows start at their local origin; the viewport owns their inset.
    /// Rows and controls share one projection of the viewer's disclosure state.
    fn feed_body(
        &self,
        _width: u16,
        _expanded: &HashSet<String>,
        _mode: OutputMode,
    ) -> ActivityParts {
        ActivityParts::default()
    }
}

pub(super) fn feed_entry(cell: &dyn HistoryCell) -> Option<&dyn FeedEntry> {
    let any = cell.as_any();
    if let Some(arrival) = any.downcast_ref::<ArrivalCell>() {
        return Some(arrival);
    }
    if let Some(preamble) = any.downcast_ref::<PreambleCell>() {
        return Some(preamble);
    }
    if let Some(alarm) = any.downcast_ref::<crate::history_cell::AlarmSoundingCell>() {
        return Some(alarm);
    }
    any.downcast_ref::<crate::history_cell::CompactionCell>()
        .map(|compaction| compaction as &dyn FeedEntry)
}

pub(super) fn entry_body(
    cell: &dyn HistoryCell,
    width: u16,
    expanded: &HashSet<String>,
    mode: OutputMode,
) -> Option<ActivityParts> {
    feed_entry(cell).map(|entry| entry.feed_body(width, expanded, mode))
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
    contribution: Contribution,
}

/// What a member brings to its item.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Contribution {
    /// A call or other activity; the first in a header's scope titles it.
    Activity,
    /// An assistant message, shown even while an item that made calls is folded.
    Message,
    /// A Thought with text to read.
    Thought,
    /// A Thought with nothing to read: only its tokens count, in its step's figures.
    Silent,
}

impl Contribution {
    fn of(cell: &dyn HistoryCell) -> Self {
        let any = cell.as_any();
        if let Some(thought) = any.downcast_ref::<ThoughtCell>() {
            if thought.is_readable() {
                Self::Thought
            } else {
                Self::Silent
            }
        } else if any.is::<AgentMarkdownCell>() {
            Self::Message
        } else {
            Self::Activity
        }
    }
}

struct Item {
    kind: Kind,
    fold: String,
    /// Records the reader's deviation from this kind's default disclosure.
    fold_mark: String,
    steps: Vec<String>,
    segments: Vec<Vec<usize>>,
}

#[derive(Clone, PartialEq, Eq)]
enum Kind {
    /// One or more steps; `calls` when any of them made a call.
    Steps { calls: bool },
    /// The step whose seal admitted the input, and the input's place among its user inputs.
    User { admitted: Option<(String, usize)> },
    Entry {
        initial_disclosure: HeaderDisclosure,
    },
    /// A Code Mode invocation no step claims.
    CodeMode,
    /// A finished reply no step claims.
    Reply,
}

impl Item {
    /// The saved mark toggles the entry's initial disclosure policy.
    fn unfolded(&self, expanded: &HashSet<String>) -> bool {
        expanded.contains(&self.fold_mark)
            == matches!(
                self.kind,
                Kind::Entry {
                    initial_disclosure: HeaderDisclosure::Collapsed
                }
            )
    }

    /// User turns and replies stand apart.
    fn spacious(&self) -> bool {
        matches!(
            self.kind,
            Kind::User { .. } | Kind::Reply | Kind::Steps { calls: false }
        )
    }

    /// Whether the member draws rows of its own.
    fn shows(&self, member: &Membership, unfolded: bool) -> bool {
        match member.contribution {
            Contribution::Silent => false,
            Contribution::Message => unfolded || self.kind == Kind::Steps { calls: true },
            Contribution::Activity | Contribution::Thought => unfolded,
        }
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
    /// The cell's rows after its first, which its item's header shows as its title; its
    /// details when nothing else remains.
    Beheaded,
    /// A feed entry's body.
    Body,
}

/// A header row above a member.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Header {
    item: usize,
    /// The members its clock comes from.
    scope: Vec<usize>,
    /// The item's steps it bills.
    steps: Vec<usize>,
    title: Title,
    disclosure: HeaderDisclosure,
    pub(super) spacious: bool,
}

pub(super) struct HeaderPreparation {
    feed: FeedIndex,
    cells: Vec<Arc<dyn HistoryCell>>,
    header: Header,
    bills: StepBills,
}

impl HeaderPreparation {
    pub(super) fn sources(&self) -> impl Iterator<Item = &dyn HistoryCell> {
        self.cells.iter().map(AsRef::as_ref)
    }

    pub(super) fn render(
        &self,
        output_tokens: &tool_output_tokens::ToolOutputTokens,
        expanded: &HashSet<String>,
        mode: OutputMode,
        invocations: &invocation::Invocations,
        full: bool,
        width: u16,
    ) -> HeaderRows {
        self.feed.header_rows(
            &self.cells,
            &self.header,
            &self.bills,
            output_tokens,
            expanded,
            mode,
            invocations,
            full,
            width,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum HeaderDisclosure {
    FixedOpen,
    Expanded,
    Collapsed,
}

impl HeaderDisclosure {
    fn unfolded(self) -> bool {
        self != Self::Collapsed
    }
}

/// What a steps header shows as its title.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Title {
    /// The first row of an activity under this header, which yields the row to it.
    Owned(usize),
    /// The first row of the item's first activity, which another header owns.
    Borrowed(usize),
    /// Nothing the item did besides respond.
    Response,
    Command,
}

/// Where one header draws, what it covers and what titles it.
struct Plan {
    site: usize,
    scope: Vec<usize>,
    steps: Vec<usize>,
    title: Title,
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
    pub(super) fn prepare_header(
        &self,
        cells: &[Arc<dyn HistoryCell>],
        header: &Header,
        bills: &StepBills,
    ) -> HeaderPreparation {
        let item = &self.items[header.item];
        let mut sources = Vec::new();
        let mut remap = |index: usize| {
            let next = sources.len();
            sources.push(Arc::clone(&cells[index]));
            next
        };
        let mut header = header.clone();
        header.item = 0;
        header.scope = header.scope.iter().map(|index| remap(*index)).collect();
        header.title = match header.title {
            Title::Owned(index) => Title::Owned(remap(index)),
            Title::Borrowed(index) => Title::Borrowed(remap(index)),
            title => title,
        };
        let mut billed_steps = item.steps.clone();
        if let Kind::User {
            admitted: Some((step, _)),
        } = &item.kind
        {
            billed_steps.push(step.clone());
        }
        billed_steps.extend(sources.iter().filter_map(|cell| {
            cell.as_any()
                .downcast_ref::<ArrivalCell>()
                .map(|arrival| arrival.response_id().to_owned())
        }));
        HeaderPreparation {
            feed: FeedIndex {
                items: vec![Item {
                    kind: item.kind.clone(),
                    fold: item.fold.clone(),
                    fold_mark: item.fold_mark.clone(),
                    steps: item.steps.clone(),
                    segments: Vec::new(),
                }],
                placed_users: item
                    .steps
                    .iter()
                    .filter_map(|id| self.placed_users.get(id).map(|count| (id.clone(), *count)))
                    .collect(),
                ..Self::default()
            },
            cells: sources,
            header,
            bills: bills.subset(&billed_steps),
        }
    }

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
            if let Some((kind, key)) =
                entry_kind(cell, &cell_nodes[index], last_step, &mut placed_users)
            {
                items.push(Item::new(kind, key));
                let item = items.len() - 1;
                items[item].segments.push(vec![index]);
                roles[index] = Role::Member(Membership {
                    item,
                    segment: 0,
                    steps: Vec::new(),
                    opens: Vec::new(),
                    contribution: Contribution::of(cell.as_ref()),
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
                let position = if let Some(position) = items[item].steps.iter().position(|step| step == name) { position } else {
                    items[item].steps.push(name.to_string());
                    opens.push(items[item].steps.len() - 1);
                    items[item].steps.len() - 1
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
                contribution: Contribution::of(cell.as_ref()),
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
        }

        self.folds = items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.fold.clone(), index))
            .collect();
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
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        width: u16,
        expanded: &HashSet<String>,
        elided: &dyn Fn(usize) -> bool,
    ) -> Seat {
        let Some(Role::Member(member)) = self.roles.get(index) else {
            return Seat::LOOSE;
        };
        let item = &self.items[member.item];
        let disclosure = if !self.foldable(item, cells, width, elided) {
            HeaderDisclosure::FixedOpen
        } else if item.unfolded(expanded) {
            HeaderDisclosure::Expanded
        } else {
            HeaderDisclosure::Collapsed
        };
        let unfolded = disclosure.unfolded();
        let header = |scope: Vec<usize>, steps: Vec<usize>, title: Title| Header {
            item: member.item,
            scope,
            steps,
            title,
            disclosure,
            spacious: item.spacious(),
        };
        if !matches!(item.kind, Kind::Steps { .. }) {
            return Seat {
                header: Some(header(
                    vec![index],
                    Vec::new(),
                    if item.kind == Kind::CodeMode {
                        Title::Owned(index)
                    } else {
                        Title::Response
                    },
                )),
                content: match (&item.kind, unfolded) {
                    (_, false) => Content::Hidden,
                    (Kind::User { .. } | Kind::Reply, true) => Content::Own,
                    (Kind::CodeMode, true) => Content::Beheaded,
                    (_, true) => Content::Body,
                },
            };
        }
        let plans = self.plans(
            item,
            cells,
            &item.segments[member.segment],
            unfolded,
            elided,
        );
        let content = if !item.shows(member, unfolded) {
            Content::Hidden
        } else if plans.iter().any(|plan| plan.title == Title::Owned(index)) {
            Content::Beheaded
        } else {
            Content::Own
        };
        let header = plans
            .into_iter()
            .find(|plan| plan.site == index)
            .map(|plan| header(plan.scope, plan.steps, plan.title));
        Seat { header, content }
    }

    /// A title and independent source handles alone do not earn a parent fold.
    fn foldable(
        &self,
        item: &Item,
        cells: &[Arc<dyn HistoryCell>],
        width: u16,
        elided: &dyn Fn(usize) -> bool,
    ) -> bool {
        if let Kind::Entry { initial_disclosure } = item.kind {
            return initial_disclosure != HeaderDisclosure::FixedOpen;
        }
        if !matches!(item.kind, Kind::Steps { calls: true }) || item.steps.len() > 1 {
            return true;
        }
        let calls: HashSet<_> = item
            .segments
            .iter()
            .flatten()
            .flat_map(|index| {
                crate::history_cell::invocation::collect(cells[*index].as_ref())
                    .into_iter()
                    .map(|call| call.id)
            })
            .collect();
        if calls.len() == 1 {
            return item.segments.iter().flatten().any(|index| {
                cells[*index]
                    .as_any()
                    .is::<CodeModeCell>()
            });
        }
        let content_width =
            crate::transcript_geometry::content_width(width, HistoryRenderMode::Rich);
        let (title_width, _) = header_columns(width);
        // Independent source/result folds must not make the parent's affordance move or
        // force materialization of their potentially large captures during seat lookup.
        let compact = HashSet::new();
        for segment in &item.segments {
            let plans = self.plans(item, cells, segment, true, elided);
            if plans.len() != self.plans(item, cells, segment, false, elided).len() {
                return true;
            }
            for &index in segment {
                let Some(Role::Member(member)) = self.roles.get(index) else {
                    continue;
                };
                if elided(index) || !item.shows(member, true) || item.shows(member, false) {
                    continue;
                }
                if !plans.iter().any(|plan| plan.title == Title::Owned(index)) {
                    return true;
                }
                let Some(cell) = cells.get(index) else {
                    continue;
                };
                if cell.activity_disclosure(content_width).is_some()
                    || title_row(cell.as_ref(), &compact, width)
                        .is_some_and(|title| line_width(&title) > title_width)
                {
                    return true;
                }
                let mut lines = cell
                    .output_parts(content_width, 0, &compact, OutputMode::Raw)
                    .map_or_else(
                        || cell.compact_hyperlink_lines(content_width),
                        |parts| parts.lines,
                    );
                behead(&mut lines);
                if !lines.is_empty() {
                    return true;
                }
            }
        }
        false
    }

    /// The headers of one segment of a steps item, in order.
    fn plans(
        &self,
        item: &Item,
        cells: &[Arc<dyn HistoryCell>],
        segment: &[usize],
        unfolded: bool,
        elided: &dyn Fn(usize) -> bool,
    ) -> Vec<Plan> {
        let member = |index: usize| match self.roles.get(index) {
            Some(Role::Member(member)) => Some(member),
            _ => None,
        };
        let opened = self.segment_steps(segment);
        let shown = segment.iter().any(|index| {
            !elided(*index) && member(*index).is_some_and(|member| item.shows(member, unfolded))
        });
        if !(unfolded && item.steps.len() > 1) {
            if opened.is_empty() && !shown {
                return Vec::new();
            }
            return segment
                .iter()
                .copied()
                .find(|index| !elided(*index))
                .map(|site| self.plan(item, cells, site, segment.to_vec(), opened, elided))
                .into_iter()
                .collect();
        }
        // Each step heads its first member. Members of steps begun in an earlier segment are
        // headed by the segment's first member, when any of them shows.
        let mut plans = Vec::new();
        let mut pending = Vec::new();
        for &candidate in segment {
            if let Some(member) = member(candidate) {
                pending.extend(member.opens.iter().copied());
            }
            if elided(candidate) || (pending.is_empty() && !(plans.is_empty() && shown)) {
                continue;
            }
            let steps = std::mem::take(&mut pending);
            let scope = segment
                .iter()
                .copied()
                .filter(|index| {
                    member(*index).is_some_and(|member| {
                        if steps.is_empty() {
                            member.steps.iter().all(|step| !opened.contains(step))
                        } else {
                            member.steps.iter().any(|step| steps.contains(step))
                        }
                    })
                })
                .collect();
            plans.push(self.plan(item, cells, candidate, scope, steps, elided));
        }
        plans
    }

    /// A header over `scope`, titled by its first activity, else by the item's.
    fn plan(
        &self,
        item: &Item,
        cells: &[Arc<dyn HistoryCell>],
        site: usize,
        scope: Vec<usize>,
        steps: Vec<usize>,
        elided: &dyn Fn(usize) -> bool,
    ) -> Plan {
        let activity = |index: &usize| {
            !elided(*index)
                && matches!(self.roles.get(*index), Some(Role::Member(member))
                    if member.contribution == Contribution::Activity)
        };
        let title = scope
            .iter()
            .copied()
            .find(activity)
            .map(Title::Owned)
            .or_else(|| {
                item.segments
                    .iter()
                    .flatten()
                    .copied()
                    .find(activity)
                    .map(Title::Borrowed)
            })
            .unwrap_or(Title::Response);
        let calls: HashSet<_> = scope
            .iter()
            .flat_map(|index| {
                crate::history_cell::invocation::collect(cells[*index].as_ref())
                    .into_iter()
                    .map(|call| call.id)
            })
            .collect();
        let title = if calls.len() > 1 {
            Title::Command
        } else {
            title
        };
        Plan {
            site,
            scope,
            steps,
            title,
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

    pub(super) fn input_open(&self, cells: &[Arc<dyn HistoryCell>], index: usize) -> bool {
        let Some(Role::Member(member)) = self.roles.get(index) else {
            return true;
        };
        let item = &self.items[member.item];
        let calls: HashSet<_> = item.segments.iter().flatten().filter(|index| {
            matches!(self.roles.get(**index), Some(Role::Member(other)) if other.steps.iter().any(|step| member.steps.contains(step)))
        }).flat_map(|index| crate::history_cell::invocation::collect(cells[*index].as_ref()).into_iter().map(|call| call.id)).collect();
        calls.len() <= 1
    }

    /// Whether the cell belongs to an item drawn as a compact list.
    pub(super) fn compact(&self, index: usize) -> bool {
        matches!(self.roles.get(index), Some(Role::Member(member))
            if !self.items[member.item].spacious())
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

    /// Fold an unfolded item, or unfold a folded one.
    pub(super) fn toggle(&self, fold: &str, expanded: &mut HashSet<String>) -> Option<usize> {
        let index = *self.folds.get(fold)?;
        let item = &self.items[index];
        if !expanded.remove(&item.fold_mark) {
            expanded.insert(item.fold_mark.clone());
        }
        Some(index)
    }

    /// The header that remains nearest `near` in its segment of the item, else the item's
    /// first.
    pub(super) fn header_site(
        &self,
        item: usize,
        cells: &[Arc<dyn HistoryCell>],
        near: usize,
        expanded: &HashSet<String>,
        elided: &dyn Fn(usize) -> bool,
    ) -> Option<usize> {
        let entry = self.items.get(item)?;
        if !matches!(entry.kind, Kind::Steps { .. }) {
            return entry.segments.first()?.first().copied();
        }
        let unfolded = entry.unfolded(expanded);
        let sites = |segment: &[usize]| -> Vec<usize> {
            self.plans(entry, cells, segment, unfolded, elided)
                .into_iter()
                .map(|plan| plan.site)
                .collect()
        };
        if let Some(Role::Member(member)) = self.roles.get(near)
            && member.item == item
        {
            let sites = sites(&entry.segments[member.segment]);
            if let Some(site) = sites
                .iter()
                .rev()
                .find(|site| **site <= near)
                .or(sites.first())
            {
                return Some(*site);
            }
        }
        entry
            .segments
            .iter()
            .find_map(|segment| sites(segment).first().copied())
    }

    pub(super) fn header_rows(
        &self,
        cells: &[Arc<dyn HistoryCell>],
        header: &Header,
        bills: &StepBills,
        output_tokens: &tool_output_tokens::ToolOutputTokens,
        expanded: &HashSet<String>,
        mode: OutputMode,
        invocations: &invocation::Invocations,
        full: bool,
        width: u16,
    ) -> HeaderRows {
        use crate::transcript_geometry::CLOCK_COLUMNS;
        use crate::transcript_geometry::TITLE_COLUMN;
        let item = &self.items[header.item];
        let clock = header
            .scope
            .iter()
            .find_map(|index| cells.get(*index)?.transcript_timestamp())
            .or_else(|| {
                header.steps.iter().find_map(|step| {
                    HistoryTimestamp::from_millis(bills.step(&item.steps[*step])?.completed_at_ms)
                })
            });
        let (title_width, reach) = header_columns(width);
        let invocation = match header.title {
            Title::Owned(index) => cells.get(index).and_then(|cell| {
                let mut calls = crate::history_cell::invocation::collect(cell.as_ref());
                (calls.len() == 1).then(|| calls.remove(0))
            }),
            _ => None,
        };
        let (titles, mut controls) = if let Some(call) = &invocation {
            if header.disclosure.unfolded() {
                let source = match header.title {
                    Title::Owned(index) => {
                        invocation::PreparationSource::Retained(&cells[index])
                    }
                    _ => unreachable!("invocation header requires an owned source"),
                };
                let prepared = invocations.prepared(call, source);
                let input_width = width
                    .saturating_sub(TITLE_COLUMN + crate::transcript_geometry::ENTRY_INDENT)
                    .max(1);
                let rows = invocations.header(
                    call,
                    &prepared,
                    input_width,
                    true,
                    title_width as u16,
                    None,
                    full,
                );
                (rows.lines, rows.controls)
            } else {
                (
                    vec![{
                        let title = truncate_line_with_ellipsis_if_overflow(
                            crate::render::header::paint_work(
                                call.title(invocations.input(call.id)),
                                call.is_active(),
                            ),
                            title_width,
                        );
                        let end = title.width();
                        HeaderRows::label(title, 0..end)
                    }],
                    Vec::new(),
                )
            }
        } else {
            let mut title = self.title(cells, header, expanded, width);
            let label_end = title.width().min(title_width);
            let mut controls = Vec::new();
            if let Title::Owned(index) = header.title
                && header.disclosure.unfolded()
                && cells[index].has_formatted_output()
            {
                for (choice, label) in [(OutputMode::Raw, "[Raw]"), (OutputMode::Fmt, "[Fmt]")] {
                    title.push_span(" ");
                    let start = title.width() as u16;
                    title.push_span(if mode == choice {
                        Span::styled(label, crate::render::disclosure::tab_style())
                    } else {
                        label.dim()
                    });
                    controls.push(HeaderControl {
                        row: 0,
                        columns: start..title.width() as u16,
                        target: FoldTarget::OutputMode(choice),
                    });
                }
            }
            (
                vec![HeaderRows::label(
                    truncate_line_with_ellipsis_if_overflow(title, title_width),
                    0..label_end,
                )],
                controls,
            )
        };
        for control in &mut controls {
            control.columns.start += TITLE_COLUMN;
            control.columns.end += TITLE_COLUMN;
        }
        let mut titles = titles.into_iter();
        let mut first = titles
            .next()
            .unwrap_or_else(|| HeaderRows::label(Line::default(), 0..0));
        let used = first.line.width();
        let mut spans = vec![
            clock.map_or_else(
                || " ".repeat(usize::from(CLOCK_COLUMNS)).into(),
                HistoryTimestamp::span,
            ),
            " ".repeat(usize::from(TITLE_COLUMN - CLOCK_COLUMNS)).into(),
        ];
        if let Some(source) = &mut first.source {
            source.prefix_bytes += spans.iter().map(|span| span.content.len()).sum::<usize>();
        }
        spans.extend(first.line.spans);
        if let Some(figures) = self.figures(cells, header, bills, output_tokens) {
            spans.push(" ".repeat(title_width - used.min(title_width) + 1).into());
            spans.push(total_span(figures.total));
            if reach >= Reach::Split
                && let Some(split) = figures.split
            {
                spans.extend(split_spans(split));
                if reach == Reach::Legend {
                    spans.push(format!("  {LEGEND}").dim());
                }
            }
            if !figures.notes.is_empty() {
                spans.push(format!("  {}", figures.notes.join(" · ")).dim());
            }
        }
        first.line = Line::from(spans);
        let mut lines = vec![first];
        lines.extend(titles.map(|mut title| {
            let mut spans = vec![" ".repeat(usize::from(TITLE_COLUMN)).into()];
            if let Some(source) = &mut title.source {
                source.prefix_bytes += usize::from(TITLE_COLUMN);
            }
            spans.extend(title.line.spans);
            title.line = Line::from(spans);
            title
        }));
        HeaderRows {
            lines,
            fold: (header.disclosure != HeaderDisclosure::FixedOpen)
                .then(|| (self.fold_target(header), header.disclosure.unfolded())),
            controls,
        }
    }

    fn title(
        &self,
        cells: &[Arc<dyn HistoryCell>],
        header: &Header,
        expanded: &HashSet<String>,
        width: u16,
    ) -> Line<'static> {
        let item = &self.items[header.item];
        let cell = |index: usize| cells.get(index).map(AsRef::as_ref);
        let response = || Line::from("Response".bold());
        let title = match (&item.kind, header.title) {
            (_, Title::Command) => Line::from("Command".bold()),
            (Kind::User { .. }, _) => Line::from("User".bold()),
            (Kind::Entry { .. }, _) => header
                .scope
                .first()
                .and_then(|index| feed_entry(cell(*index)?))
                .map(FeedEntry::feed_title)
                .unwrap_or_default(),
            (Kind::Steps { .. } | Kind::CodeMode, Title::Owned(index) | Title::Borrowed(index)) => {
                cell(index)
                    .and_then(|cell| title_row(cell, expanded, width))
                    .unwrap_or_else(response)
            }
            (Kind::Steps { .. } | Kind::CodeMode, Title::Response) | (Kind::Reply, _) => response(),
        };
        if matches!(item.kind, Kind::Steps { calls: true } | Kind::CodeMode) {
            let active = header
                .scope
                .iter()
                .filter_map(|index| cell(*index))
                .any(crate::history_cell::has_active_invocations);
            crate::render::header::paint_work(title, active)
        } else {
            crate::render::header::paint(title)
        }
    }

    fn figures(
        &self,
        cells: &[Arc<dyn HistoryCell>],
        header: &Header,
        bills: &StepBills,
        output_tokens: &tool_output_tokens::ToolOutputTokens,
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
            Kind::Entry { .. } => {
                let cell = cell?;
                if let Some(arrival) = cell.as_any().downcast_ref::<ArrivalCell>() {
                    arrival_figures(
                        bills.step(arrival.response_id()),
                        arrival.id(),
                        arrival.estimated_tokens(),
                    )
                } else {
                    Figures {
                        total: Total::Exact(cell.activity_header_tokens()?),
                        ..Figures::default()
                    }
                }
            }
            Kind::CodeMode => Figures {
                // Unadmitted entries can still own a measured model-visible return.
                total: Total::Exact(output_tokens.for_ids(&cell?.activity_ids())?),
                ..Figures::default()
            },
            Kind::Reply => return None,
        };
        (figures.total != Total::Unknown).then_some(figures)
    }
}

impl Item {
    fn new(kind: Kind, key: String) -> Self {
        let fold = format!("feed:{key}");
        Self {
            kind,
            fold_mark: format!("{fold}-"),
            fold,
            steps: Vec::new(),
            segments: Vec::new(),
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

/// The cells that are items of their own, with their fold keys: user turns, arrivals,
/// Preambles, and finished replies no step claims.
fn entry_kind(
    cell: &Arc<dyn HistoryCell>,
    steps: &[usize],
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
    if let Some(entry) = feed_entry(cell.as_ref()) {
        return Some((
            Kind::Entry {
                initial_disclosure: entry.feed_disclosure(),
            },
            entry.feed_id(),
        ));
    }
    if steps.is_empty()
        && let Some(block) = any.downcast_ref::<CodeModeCell>()
    {
        return Some((Kind::CodeMode, block.identity()));
    }
    if steps.is_empty()
        && let Some(reply) = any.downcast_ref::<AgentMarkdownCell>()
    {
        let key = reply
            .item_id
            .as_ref()
            .map_or_else(|| format!("{:x}", cell_key(cell)), String::clone);
        return Some((Kind::Reply, format!("reply:{key}")));
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
                .step_of_call(block.exec_call_id())
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
        CodeModeEvent::WaitStarted { call_id, .. }
        | CodeModeEvent::WaitCompleted { call_id, .. } => Some((call_id.as_str(), exec)),
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

/// The row an activity yields to its header: its first, clipped where the activity would
/// clip it, without its marker.
fn title_row(
    cell: &dyn HistoryCell,
    expanded: &HashSet<String>,
    width: u16,
) -> Option<Line<'static>> {
    let width = crate::transcript_geometry::content_width(width, HistoryRenderMode::Rich);
    let lines = match cell.output_parts(width, /*reserve*/ 0, expanded, OutputMode::Raw) {
        Some(parts) => parts.lines,
        None => cell.compact_hyperlink_lines(width),
    };
    let row = lines.into_iter().find(|line| line.width() > 0)?;
    Some(strip_marker(compact_header(row.line, usize::from(width))))
}

/// Drop the row an activity yields to its header, with the blank rows around it; how many
/// rows went.
pub(super) fn behead(lines: &mut Vec<HyperlinkLine>) -> usize {
    let Some(title) = lines.iter().position(|line| line.width() > 0) else {
        return 0;
    };
    let rest = lines[title + 1..]
        .iter()
        .position(|line| line.width() > 0)
        .map_or(lines.len(), |rest| title + 1 + rest);
    lines.drain(..rest);
    rest
}

/// A header's total, right-aligned.
const TOTAL_WIDTH: usize = 6;
/// The columns each part of a step's split takes, shown or not: thought, output, self.
const SPLIT: [usize; 3] = [5, 5, 7];
const SPLIT_WIDTH: usize = 2 * SPLIT.len() + SPLIT[0] + SPLIT[1] + SPLIT[2];
/// What the split's columns hold, said after them.
const LEGEND: &str = "(thought, output, self)";
const MIN_TITLE: usize = 16 + invocation::CONTROL_COLUMNS;
const MAX_TITLE: usize = 48 + invocation::CONTROL_COLUMNS;

/// How far a header's figures reach along its row.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Reach {
    Total,
    Split,
    Legend,
}

impl Reach {
    const fn width(self) -> usize {
        match self {
            Self::Total => TOTAL_WIDTH,
            Self::Split => TOTAL_WIDTH + SPLIT_WIDTH,
            Self::Legend => TOTAL_WIDTH + SPLIT_WIDTH + 2 + LEGEND.len(),
        }
    }
}

/// How a header divides its row: the title's columns, and how far the figures after it reach.
/// Every header at one width divides it alike, so their figures line up.
pub(super) fn header_columns(width: u16) -> (usize, Reach) {
    let room = usize::from(width)
        .saturating_sub(usize::from(crate::transcript_geometry::TITLE_COLUMN) + 1);
    let reach = [Reach::Legend, Reach::Split]
        .into_iter()
        .find(|reach| room >= MIN_TITLE + reach.width())
        .unwrap_or(Reach::Total);
    (room.saturating_sub(reach.width()).min(MAX_TITLE), reach)
}

/// The split in its fixed columns, each part colored by its size like the total; a part with
/// nothing to report leaves its column blank.
fn split_spans(split: Split) -> impl Iterator<Item = Span<'static>> {
    let [thought, output, own] = SPLIT;
    let measured = |tokens: u64, width: usize| -> Span<'static> {
        if tokens == 0 {
            " ".repeat(width).into()
        } else {
            token_figure(token_label(tokens), tokens, width)
        }
    };
    let own = match split.own {
        Own::Pending => format!("{:>own$}", "…").dim(),
        Own::Severed => format!("{:>own$}", "—").dim(),
        Own::Measured {
            tokens,
            approximate,
            pending,
        } if tokens > 0 || pending => {
            let approximate = if approximate { "≈" } else { "" };
            let pending = if pending { "+…" } else { "" };
            token_figure(
                format!("{approximate}{}{pending}", token_label(tokens)),
                tokens,
                own,
            )
        }
        Own::Measured { .. } => " ".repeat(own).into(),
    };
    [
        measured(split.thought, thought),
        measured(split.output, output),
        own,
    ]
    .into_iter()
    .flat_map(|figure| [Span::from("  "), figure])
}

/// The total, right-aligned and colored by size; placeholders stay dim.
fn total_span(total: Total) -> Span<'static> {
    match total {
        Total::Exact(tokens) => token_figure(token_label(tokens), tokens, TOTAL_WIDTH),
        Total::Approximate(tokens) => {
            token_figure(format!("≈{}", token_label(tokens)), tokens, TOTAL_WIDTH)
        }
        Total::Pending => format!("{:>TOTAL_WIDTH$}", "…").dim(),
        Total::Severed => format!("{:>TOTAL_WIDTH$}", "—").dim(),
        Total::Unknown => " ".repeat(TOTAL_WIDTH).into(),
    }
}

impl TranscriptView {
    pub(crate) fn record_step_admission(&mut self, item: StepAdmissionItem) {
        if self.step_bills.record(item) {
            self.cache.clear();
            self.live_key = None;
        }
    }

    /// The run whose rotation seats a step's Thought ahead of the step: from the first of the
    /// step's own members before the Thought, through the Thought. The search ends at a user
    /// turn, or at a cell another step claims.
    pub(crate) fn misplaced_thought(
        &self,
        cells: &[Arc<dyn HistoryCell>],
        response_id: &str,
    ) -> Option<Range<usize>> {
        let bills = &self.step_bills;
        bills.step(response_id)?;
        let mut thought = None;
        let mut first = None;
        for (index, cell) in cells.iter().enumerate().rev() {
            let any = cell.as_any();
            if any.is::<UserHistoryCell>() {
                break;
            }
            if let Some(candidate) = any.downcast_ref::<ThoughtCell>() {
                if thought.is_none() && candidate.response_id() == response_id {
                    thought = Some(index);
                }
                continue;
            }
            if thought.is_none() {
                continue;
            }
            match steps_of(cell.as_ref(), bills).0.as_slice() {
                [] => {}
                [step] if *step == response_id => first = Some(index),
                _ => break,
            }
        }
        Some(first?..thought? + 1)
    }

    /// Seats every Thought of a page of cells ahead of its step.
    pub(crate) fn seat_thoughts(&self, cells: &mut [Arc<dyn HistoryCell>]) {
        for index in 0..cells.len() {
            let Some(response_id) = cells[index]
                .as_any()
                .downcast_ref::<ThoughtCell>()
                .map(|thought| thought.response_id().to_string())
            else {
                continue;
            };
            if let Some(range) = self.misplaced_thought(&cells[..=index], &response_id) {
                cells[range].rotate_right(/*k*/ 1);
            }
        }
    }

    /// The cell's place in the feed; loose wherever the feed is not drawn.
    pub(super) fn feed_seat(&mut self, cells: &[Arc<dyn HistoryCell>], index: usize) -> Seat {
        if self.detailed || self.mode != HistoryRenderMode::Rich || index >= cells.len() {
            return Seat::LOOSE;
        }
        self.refresh_feed(cells, index);
        let seat = self.feed.seat(
            cells,
            index,
            self.area.width.max(1),
            &self.disclosure.expanded,
            &|index| self.elides_cell(cells, index),
        );
        if let Some(Header {
            item,
            disclosure: HeaderDisclosure::FixedOpen,
            ..
        }) = &seat.header
        {
            self.disclosure
                .expanded
                .remove(&self.feed.items[*item].fold_mark);
        }
        seat
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
        self.feed.seat(
            cells,
            index,
            self.area.width.max(1),
            &self.disclosure.expanded,
            &|index| self.elides_cell(cells, index),
        )
    }

    pub(super) fn refresh_feed(&mut self, cells: &[Arc<dyn HistoryCell>], index: usize) {
        let frame = self.cache.frame();
        if !self
            .feed
            .verify(cells, index, frame, self.step_bills.revision())
        {
            self.feed.rebuild(cells, &self.step_bills, frame);
            for (index, cell) in cells.iter().enumerate() {
                let Some(block) = cell.as_any().downcast_ref::<CodeModeCell>() else {
                    continue;
                };
                let Some(Role::Member(member)) = self.feed.roles.get(index) else {
                    continue;
                };
                let item = &self.feed.items[member.item];
                if !matches!(item.kind, Kind::Steps { .. }) {
                    continue;
                }
                // Both forms default open; late admission moves the same closed mark and focus.
                let prior = Item::new(Kind::CodeMode, block.identity());
                if self.disclosure.expanded.remove(&prior.fold_mark) {
                    self.disclosure.expanded.insert(item.fold_mark.clone());
                }
                if let Some(focused) = &mut self.disclosure.focused
                    && let FoldTarget::Part(fold) = &mut focused.target
                    && *fold == prior.fold
                {
                    *fold = item.fold.clone();
                }
            }
        }
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
        let site = self
            .feed
            .header_site(item, cells, index, &self.disclosure.expanded, &|index| {
                self.elides_cell(cells, index)
            })
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
        self.disclosure.focused = Some(disclosure::ActivityFocus {
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

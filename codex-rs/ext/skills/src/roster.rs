//! Skills world-state sections that amend the model's catalog instead of replacing it.
//!
//! The first complete catalog a section shows opens the model's listing, and the persisted
//! roster records what that listing and every later amendment told the model. Later steps
//! append only added, changed, and removed entries, quoted as unaliased lines so the alias table
//! of the opening catalog stays valid. A catalog that omits entries under budget pressure cannot
//! be amended, because the model never saw the omitted entries, so it is replaced whole.

use std::collections::BTreeMap;
use std::collections::HashSet;

use codex_extension_api::ContextualUserFragment;
use codex_extension_api::PreviousWorldStateSection;
use codex_extension_api::RenderedWorldStateFragment;
use codex_extension_api::WorldStateSectionContribution;
use codex_protocol::protocol::SKILLS_INSTRUCTIONS_CLOSE_TAG;
use codex_protocol::protocol::SKILLS_INSTRUCTIONS_OPEN_TAG;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

use crate::render::AvailableSkillsRender;
use crate::render::SkillRenderReport;
use crate::world_state_catalogs::CatalogBudgetAllocation;

pub(crate) const CLOUD_SKILLS_WORLD_STATE_ID: &str = "cloud_skills";
const UNCHANGED_NOTE: &str = "Skills not named below are unchanged.";
const OMITTED_HOST_SKILLS: &str = "Host skills are available but omitted from the model-visible skills list because the skills context budget was exceeded.";

pub(crate) type CatalogRenderCallback = Box<dyn Fn() + Send + Sync>;

/// Model-visible wording of one section.
struct Voice {
    id: &'static str,
    heading: &'static str,
    nothing: &'static str,
    hidden: &'static str,
    restored: &'static str,
}

static EXECUTOR: Voice = Voice {
    id: "skills",
    heading: "## Skills update",
    nothing: "No selected-environment skills are currently available.",
    hidden: "Selected-environment skills are not listed automatically. Explicit skill mentions can still be resolved when available.",
    restored: "The previously listed selected-environment skills are available again.",
};

static CLOUD: Voice = Voice {
    id: CLOUD_SKILLS_WORLD_STATE_ID,
    heading: "## Cloud skills update",
    nothing: "No cloud skills are currently available.",
    hidden: "Cloud skills are not listed automatically. Explicit skill mentions can still be resolved when available.",
    restored: "The previously listed cloud skills are available again.",
};

static HOST: Voice = Voice {
    id: "host_skills",
    heading: "## Host skills update",
    nothing: "No host skills are currently available.",
    hidden: "Host skills are not listed automatically. Explicit skill mentions can still be resolved when available.",
    restored: "The previously listed host skills are available again.",
};

/// What one section can show the model on this step.
pub(crate) enum Content {
    /// No catalog: the section stays silent or posts a notice.
    Unlisted,
    /// A catalog that omits entries, so only identical text leaves it current.
    Bounded(String),
    /// A catalog of every entry, which later steps may amend.
    Complete(Catalog),
}

pub(crate) struct Catalog {
    body: String,
    muster: Muster,
    /// Whether the catalog carries the usage instructions.
    usage: bool,
    /// Whether the catalog shortens descriptions to fit its budget.
    abridged: bool,
}

impl Content {
    pub(crate) fn new(
        rendered: Option<AvailableSkillsRender>,
        muster: Muster,
        usage: bool,
    ) -> Self {
        let Some(rendered) = rendered else {
            return Self::Unlisted;
        };
        let SkillRenderReport {
            omitted_count,
            truncated_description_count,
            ..
        } = rendered.report;
        match rendered.into_fragment(usage) {
            None => Self::Unlisted,
            Some(fragment) if omitted_count == 0 => Self::Complete(Catalog {
                body: fragment.body(),
                muster,
                usage,
                abridged: truncated_description_count > 0,
            }),
            Some(fragment) => Self::Bounded(fragment.body()),
        }
    }
}

/// One catalog entry as amendments quote it.
pub(crate) struct Enlistment {
    /// The full unaliased catalog line.
    line: String,
    /// The line without its description, which identifies the entry.
    cite: String,
    digest: Digest,
}

impl Enlistment {
    pub(crate) fn new(line: String, cite: String) -> Self {
        let digest = Digest::of(&line);
        Self { line, cite, digest }
    }
}

/// Every model-visible entry of one catalog, in catalog order.
pub(crate) struct Muster(Vec<Enlistment>);

impl FromIterator<Enlistment> for Muster {
    fn from_iter<I: IntoIterator<Item = Enlistment>>(enlistments: I) -> Self {
        // Two entries citing the same name and locator would contend for one roster mark.
        let mut cited = HashSet::new();
        Self(
            enlistments
                .into_iter()
                .filter(|enlistment| cited.insert(enlistment.cite.clone()))
                .collect(),
        )
    }
}

impl Muster {
    fn roster(&self) -> Roster {
        Roster(
            self.0
                .iter()
                .map(|enlistment| (enlistment.cite.clone(), enlistment.digest.clone()))
                .collect(),
        )
    }
}

/// Digest of each entry line the model was last told, keyed by the entry's cite.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct Roster(BTreeMap<String, Digest>);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct Digest(String);

impl Digest {
    fn of(text: &str) -> Self {
        Self(blake3::hash(text.as_bytes()).to_hex().to_string())
    }

    /// Digests the text a fragment with this body occupies in model history.
    fn fragment(body: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        for part in [
            SKILLS_INSTRUCTIONS_OPEN_TAG,
            body,
            SKILLS_INSTRUCTIONS_CLOSE_TAG,
        ] {
            hasher.update(part.as_bytes());
        }
        Self(hasher.finalize().to_hex().to_string())
    }
}

/// What the newest fragments of a section left the model believing.
#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
enum Shown {
    /// No listing: silence or a notice that names no entries.
    Unlisted,
    /// A catalog that omits entries.
    Bounded { digest: Digest },
    /// A complete catalog and its amendments.
    Listed(Listing),
    /// A listing superseded by an unavailability notice, restorable while history retains it.
    Withdrawn(Listing),
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listing {
    /// Whether the catalog that opened this listing carried the usage instructions.
    usage: bool,
    /// Whether the catalog that opened this listing shortened descriptions.
    abridged: bool,
    roster: Roster,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    include_instructions: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    allocation: Option<CatalogBudgetAllocation>,
    shown: Shown,
}

enum Prior {
    Absent,
    Unknown,
    Known(Shown),
}

/// What a step appends to model history.
enum Post {
    Nothing,
    /// Catalog news, whose render is recorded.
    News(String),
    /// A reminder that leaves the catalog as listed.
    Reminder(String),
}

struct Changes<'a> {
    added: Vec<&'a str>,
    changed: Vec<&'a str>,
    removed: Vec<&'a str>,
}

impl<'a> Changes<'a> {
    fn between(roster: &'a Roster, muster: &'a Muster) -> Self {
        let mut added = Vec::new();
        let mut changed = Vec::new();
        for enlistment in &muster.0 {
            match roster.0.get(&enlistment.cite) {
                None => added.push(enlistment.line.as_str()),
                Some(digest) if *digest != enlistment.digest => {
                    changed.push(enlistment.line.as_str());
                }
                Some(_) => {}
            }
        }
        let current = muster
            .0
            .iter()
            .map(|enlistment| enlistment.cite.as_str())
            .collect::<HashSet<_>>();
        let removed = roster
            .0
            .keys()
            .map(String::as_str)
            .filter(|cite| !current.contains(cite))
            .collect();
        Self {
            added,
            changed,
            removed,
        }
    }

    fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }
}

struct Section {
    voice: &'static Voice,
    include_instructions: bool,
    enabled: Option<bool>,
    allocation: Option<CatalogBudgetAllocation>,
    content: Content,
}

impl Section {
    fn contribution(self, on_render: CatalogRenderCallback) -> WorldStateSectionContribution {
        let fragment = |body| {
            RenderedWorldStateFragment::new(
                "developer",
                (SKILLS_INSTRUCTIONS_OPEN_TAG, SKILLS_INSTRUCTIONS_CLOSE_TAG),
                body,
            )
        };
        let (_, full) = self.render(PreviousWorldStateSection::Absent);
        let current = match full {
            Post::Nothing => None,
            Post::News(body) | Post::Reminder(body) => Some(fragment(body)),
        };
        WorldStateSectionContribution::new(self.voice.id, move |previous| {
            let (shown, post) = self.render(previous);
            let body = match post {
                Post::Nothing => None,
                Post::News(body) => {
                    on_render();
                    Some(body)
                }
                Post::Reminder(body) => Some(body),
            };
            (Some(self.snapshot(shown)), body.map(fragment))
        })
        .with_instruction_amendments(current)
    }

    fn render(&self, previous: PreviousWorldStateSection<'_>) -> (Shown, Post) {
        let prior = match previous {
            PreviousWorldStateSection::Absent => Prior::Absent,
            PreviousWorldStateSection::Unknown => Prior::Unknown,
            PreviousWorldStateSection::Known(previous) => match Snapshot::deserialize(previous) {
                Ok(snapshot)
                    if snapshot.include_instructions == self.include_instructions
                        && snapshot.enabled == self.enabled =>
                {
                    Prior::Known(snapshot.shown)
                }
                // Another setting or an older format describes text this step cannot amend.
                Ok(_) | Err(_) => Prior::Unknown,
            },
        };
        match &self.content {
            Content::Unlisted => match prior {
                Prior::Absent | Prior::Known(Shown::Unlisted) => (Shown::Unlisted, Post::Nothing),
                Prior::Known(withdrawn @ Shown::Withdrawn(_)) => (withdrawn, Post::Nothing),
                Prior::Known(Shown::Listed(listing)) if self.include_instructions => {
                    (Shown::Withdrawn(listing), Post::News(self.notice()))
                }
                Prior::Unknown | Prior::Known(Shown::Listed(_) | Shown::Bounded { .. }) => {
                    (Shown::Unlisted, Post::News(self.notice()))
                }
            },
            Content::Bounded(body) => {
                let digest = Digest::fragment(body);
                let post = match &prior {
                    Prior::Known(Shown::Bounded { digest: shown }) if *shown == digest => {
                        Post::Nothing
                    }
                    Prior::Absent | Prior::Unknown | Prior::Known(_) => Post::News(body.clone()),
                };
                (Shown::Bounded { digest }, post)
            }
            Content::Complete(catalog) => match prior {
                Prior::Known(Shown::Listed(listing)) => {
                    self.amend(listing, /*restored*/ false, catalog)
                }
                Prior::Known(Shown::Withdrawn(listing)) => {
                    self.amend(listing, /*restored*/ true, catalog)
                }
                Prior::Absent
                | Prior::Unknown
                | Prior::Known(Shown::Unlisted | Shown::Bounded { .. }) => list(catalog),
            },
        }
    }

    fn amend(&self, listing: Listing, restored: bool, catalog: &Catalog) -> (Shown, Post) {
        // Amendments cannot supply the usage instructions or the full descriptions that the
        // opening catalog lacked, so a catalog that would show them opens a new listing.
        if (catalog.usage && !listing.usage) || (listing.abridged && !catalog.abridged) {
            return list(catalog);
        }
        let changes = Changes::between(&listing.roster, &catalog.muster);
        if changes.is_empty() && !restored {
            return (Shown::Listed(listing), Post::Nothing);
        }
        let amendment = self.amendment(&changes, restored);
        if amendment.len() >= catalog.body.len() {
            return list(catalog);
        }
        let shown = Shown::Listed(Listing {
            usage: listing.usage,
            abridged: listing.abridged,
            roster: catalog.muster.roster(),
        });
        if changes.is_empty() {
            (shown, Post::Reminder(amendment))
        } else {
            (shown, Post::News(amendment))
        }
    }

    fn amendment(&self, changes: &Changes<'_>, restored: bool) -> String {
        let mut lines = vec![self.voice.heading];
        if restored {
            lines.push(self.voice.restored);
        }
        if !changes.is_empty() {
            lines.push(UNCHANGED_NOTE);
            for (title, entries) in [
                ("### Added skills", &changes.added),
                ("### Changed skills", &changes.changed),
                ("### Removed skills", &changes.removed),
            ] {
                if !entries.is_empty() {
                    lines.push(title);
                    lines.extend(entries.iter().copied());
                }
            }
        }
        format!("\n{}\n", lines.join("\n"))
    }

    fn notice(&self) -> String {
        let notice = if self.include_instructions || self.enabled == Some(false) {
            self.voice.nothing
        } else {
            self.voice.hidden
        };
        format!("\n{}\n{notice}\n", self.voice.heading)
    }

    fn snapshot(&self, shown: Shown) -> Value {
        json!(Snapshot {
            include_instructions: self.include_instructions,
            enabled: self.enabled,
            allocation: self.allocation,
            shown,
        })
    }
}

fn list(catalog: &Catalog) -> (Shown, Post) {
    let shown = Shown::Listed(Listing {
        usage: catalog.usage,
        abridged: catalog.abridged,
        roster: catalog.muster.roster(),
    });
    (shown, Post::News(catalog.body.clone()))
}

pub(crate) fn executor_section(
    content: Content,
    include_instructions: bool,
    on_render: CatalogRenderCallback,
) -> WorldStateSectionContribution {
    Section {
        voice: &EXECUTOR,
        include_instructions,
        enabled: None,
        allocation: None,
        content,
    }
    .contribution(on_render)
    .with_legacy_matcher(|role, text| {
        role == "developer"
            && text.trim_start().starts_with(SKILLS_INSTRUCTIONS_OPEN_TAG)
            && text.trim_end().ends_with(SKILLS_INSTRUCTIONS_CLOSE_TAG)
    })
}

pub(crate) fn cloud_section(
    content: Content,
    include_instructions: bool,
    enabled: bool,
    allocation: CatalogBudgetAllocation,
    on_render: CatalogRenderCallback,
) -> WorldStateSectionContribution {
    Section {
        voice: &CLOUD,
        include_instructions,
        enabled: Some(enabled),
        allocation: Some(allocation),
        content,
    }
    .contribution(on_render)
}

pub(crate) fn host_section(
    content: Content,
    include_instructions: bool,
    report: &SkillRenderReport,
    on_render: CatalogRenderCallback,
) -> WorldStateSectionContribution {
    let content = match content {
        Content::Unlisted if report.included_count == 0 && report.omitted_count > 0 => {
            Content::Bounded(format!("\n{}\n{OMITTED_HOST_SKILLS}\n", HOST.heading))
        }
        content => content,
    };
    Section {
        voice: &HOST,
        include_instructions,
        enabled: None,
        allocation: None,
        content,
    }
    .contribution(on_render)
}

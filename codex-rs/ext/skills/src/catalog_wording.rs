//! Terse wording of the always-visible skills catalog.

use crate::catalog::SkillSourceKind;

pub(crate) const HOST_ALIASES_INTRO: &str = "A skill is a `SKILL.md` file of local instructions to follow. Entries give each skill's name, description, and short path; expand the path's alias from the skill roots table.";
pub(crate) const SOURCE_LOCATORS_INTRO: &str = "A skill is a `SKILL.md` source of instructions. Entries give each skill's name, description, and source locator: a plain path is a `file` on the host filesystem, an `executor package` is owned by its execution environment, a `cloud package` is an opaque package identifier, and a `custom resource` uses its provider's access mechanism.";
pub(crate) const RESOURCE_ALIASES_INTRO: &str = "A skill is a `SKILL.md` source of instructions. Entries give each skill's name, description, and source locator; expand short locators from the skill roots table.";

/// Names how a locator is read; a plain path needs no name.
pub(crate) fn locator_kind(kind: &SkillSourceKind) -> Option<&'static str> {
    match kind {
        SkillSourceKind::Host => None,
        SkillSourceKind::Executor => Some("executor package"),
        SkillSourceKind::Cloud => Some("cloud package"),
        SkillSourceKind::Custom(_) => Some("custom resource"),
    }
}
